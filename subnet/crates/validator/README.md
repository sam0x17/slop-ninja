# Native validator

`slop_ninja_validator` is the native Slop Ninja validator. It builds on the
journaled adapter in [`subnet/`](../../README.md) and rtao `0c128160`
(tag `snapshot-2026-11-03.2`). It
verifies the coordinator's signed score package, derives the native row with
the shared [protocol crate](../protocol/README.md), and commits it as a
timelocked weight commit through rtao's journaled scheduler. By default it
only prepares the commit and prints its hash; `run --dispatch` signs.

With `[fetch]`, the validator fetches the coordinator's published records
from the public status host, verifies them and installs the bundle itself
([Fetcher](#fetcher)). Without it, the package source is testnet shortcut
T12: one local bundle file the operator installs. A `slop_ninja.fixture_package/v1`
bundle (built by `slop_ninja_fixture_package`, or by the coordinator's
`eval_verify`) carries a `testnet-reward-v0` package (T8), valid only on the
testnet genesis. A `slop_ninja.reward_v1_bundle/v1` bundle (written by the
coordinator's `reward_v1_verify` from the receipts bucket) carries the active
`reward-v1` target: the launch burn row under a launch authorization, or a
cohort target with its manifest, package, closure, published index, receipt
index and receipt chunks, and its reward-intent authorization. The live
procedure for subnet 601 is [TESTNET-M2.md](TESTNET-M2.md).

## Run order

`run` performs these steps against one finalized block:

1. Open the existing native journal. Opening persists pause, and `run` never
   creates a journal.
2. Connect an rtao client and require the journal's genesis.
3. Evaluate the signed weight operations against the live head metadata,
   check the storage items rtao's scheduler reads against their compiled
   descriptors, read `CommitRevealWeightsVersion` by name, and persist any
   readiness change in the journal (see
   [Runtime upgrades](../../README.md#runtime-upgrades)). A deferred operation
   is logged as `operation_deferred` with `alert = true`; it never halts the
   run. No spec or transaction version is compared anywhere. This comes
   before recovery because the adapter refuses resume after any journal
   write since the recovery that established it, and recording a first
   evaluation or a readiness change is such a write.
4. Call `recover_finalized` without a signer, in bounded rounds, until the
   adapter reports it is ready to resume. When recovery errors and the
   readiness just recorded names `upstream_storage_refused` (a storage item
   that rtao reads only through a compiled descriptor no longer matches live
   metadata), startup logs it as an alert and exits 75 to retry. Any other
   recovery error exits 1.
5. Read `NetworksAdded(netuid)`, `Uids(netuid, hotkey)` and
   `CommitRevealWeightsEnabled(netuid)` together at the finalized block.
   Startup halts if the subnet is absent (`subnet_not_registered`), the hotkey
   has no uid on it (`hotkey_not_registered`), or commit/reveal is disabled
   (`commit_reveal_disabled`). The flag defaults to enabled for a netuid that
   does not exist, so it counts only for a registered subnet. If the compiled
   storage descriptors no longer match the live metadata, the same items are
   read by name with live key types and defaults.
6. Resume explicitly through the adapter, which refuses unless this process
   completed real finalized recovery. With `[fetch]`, run one fetch session
   (bounded in time by `poll_secs`) and install what it verified, by the
   [supersession rules](#fetcher); a failed or timed-out session leaves the
   installed bundle. A dry run writes nothing: it never retires journaled
   work, so it leaves a switch that would retire queued work to the next
   dispatch run (`switch_needs_dispatch`), and a bundle it would install is
   staged in memory for its own verification (`staged_in_memory`), never
   written over `[package] path`; nor does it write the held-target record.
   With no bundle installed afterwards, a continuous dispatch run waits for
   the fetcher; any other run exits 75 (`bundle_unavailable`).
7. With a `[package]` configured, verify the bundle: open the signed cohort
   manifest, score package and calculation target (package key) and receipt
   index (receipt key), authenticating each before decoding it. Then bind the
   package to its manifest, the target to the package and the index to the
   package's receipt root and count, check every stored receipt object
   against its index entry, and require the package's genesis and netuid to
   be this validator's. A `reward-v1` bundle goes through the protocol
   crate's `target_bundle::verify`: it also requires every receipt chunk
   (each index entry stored exactly once), the published index matching the
   manifest's binding, the recomputed outcome (`check_outcome`), the closure
   against the manifest's minimums, and the authorization against the active
   target and its carrying cohort (whose outcome is recomputed too, and whose
   receipt chunks are verified). The active target is this validator's own:
   `<journal>.held-target.json` (mode 600) records the target a verified
   bundle's checked closure activated, and only that target or its successor
   is accepted afterwards; the launch bundle only until a cohort target has
   activated. Any failure is a refusal (exit 3) before a row exists.
8. Read `SubnetworkN`, `Keys` and `BlockAtRegistration` by name at the same
   finalized block into a registration snapshot, and derive the row with
   `slop_ninja_protocol::reward::derive_row`. For `reward-v1` the snapshot
   also carries the finalized `SubnetOwnerHotkey`, whose UID receives burned
   units, and the row is `reward_v1::derive_row`; the operation is the
   `WeightOperationV2` for the current `SubnetEpochIndex`, issued only under
   the authorization checked for that epoch (`authorization_refused`
   otherwise). A `reward-v1` row sums to 65,535; it is committed, and compared
   with `Weights` after its reveal, max-upscaled so that its largest entry is
   65,535 (`WeightRowV2::chain_values`), the form rtao's preparation and the
   chain's reveal both keep. Each later tempo's re-commit derives its row
   afresh from a finalized snapshot and owner hotkey and checks the
   authorization again.
9. Read `SubnetOwnerHotkey(netuid)` by name and choose the row's owner-row
   policy (rtao's `OwnerRowPolicy`, D-093). A `reward-v1` row whose every
   positive entry goes to the UID the finalized owner hotkey holds is an
   owner burn row: the launch burn row under the launch authorization, or a
   verified cohort target whose row pays only the owner UID (its units burn,
   or an entrant registered under the owner hotkey is paid there; the chain
   receives the same row either way). It is queued under `Submit`,
   which rtao journals with the work (`"owner_rows":"submit"`) and commits and
   reveals like any row. Every other row, and every `testnet-reward-v0` row,
   is queued under `Hold`, the default, whose journaled bytes are unchanged.
   The hold decision is rtao's own predicate, `OwnerRowPolicy::holds`, under
   that policy: this validator's UID is the owner's only when its own hotkey
   is the owner hotkey, and any other UID is the owner's when the finalized
   snapshot maps it to the owner hotkey. A row rtao would still hold (a
   self-only or self-plus-owner row, or an all-zero row) is deferred
   (`row_would_be_held`), so this validator commits nothing that tempo and
   its previous weights stay on chain. Before a burn row is queued, rtao's
   own weight preflight is read at the best head, where its preparation
   reads it (`MinAllowedWeights`, `MaxWeightsLimit`, `SubnetworkN`,
   `SubnetEpochConsensus` and `ValidatorPermit` among its fields; compiled
   reads, by name when the shapes no longer match): a one-entry burn row
   from a validator that is not the owner needs an effective
   `MinAllowedWeights` (capped at the population) of 1
   (`burn_row_min_allowed_weights` otherwise); under a `MaxWeightsLimit`
   below 65535 the row must list only the owner UID
   (`burn_row_weights_limit`; `reward-v1` rows list positive entries only, so
   a burn row always does); and this validator needs a validator permit
   wherever the chain's `check_validator_permit` requires one: under Yuma
   unless it is the owner validator, and always under `Null` consensus
   (`burn_row_no_permit`). rtao only warns about a missing permit, and the
   chain applies that check, the length check and the stake check
   (`StakeThreshold`, which is not checked here) again when it reveals a
   timelocked commit, where a failure drops the row with no error. Each is
   a deferral, never a queued operation that would stay pending.
   `row_would_be_held` and every `burn_row_*` deferral are logged as
   `tempo_deferred` with `alert = true`: each leaves this validator
   committing nothing that tempo. A row that must burn while the owner hotkey holds no
   UID (`row_deferred`), an authorization that yields no operation at the
   epoch (`authorization_refused`: its target is not the held one, or the
   epoch precedes its issue; never an expiry), and one a journaled signed
   revocation withdraws (`authorization_revoked`, logged with `alert = true`;
   queued work of that target that never signed is retired), defer the same
   way. A continuous dispatch run waits in the same process for the next
   epoch (with `[fetch]`, fetching every `poll_secs` and continuing at once
   when a newer authorization or successor is installed) and applies the
   bundle again;
   a dry run or bounded run exits 75. Inside the dispatch loop the tempo is
   skipped and the next epoch is tried afresh. The first commitment and each
   per-tempo re-commit resolve the label against the journal the same way
   (`app::derive_queueable`), looking up the label's retained operation
   before reading the limits. One with the same row and policy is joined
   whatever the limits read now, so a restart adopts its unresolved attempt
   or reads its landed commit back; only a pending burn row that never
   signed and that the limits now refuse is retired
   (`burn_row_limits_changed`) and the tempo deferred. When the limits refuse
   a new burn row, pending `Submit` work of the same row that never landed
   is retired the same way. An operation of this tempo with the same row but
   another policy (a burn row an earlier build queued under `Hold`) is
   retired if it never landed (`owner_row_policy_changed`) and the next label
   is used; rtao refuses the same operation ID under another policy. If the
   limits change after a burn row is queued and before it signs, rtao's
   preparation refuses the row before signing. The dispatch loop then
   alerts (`weight_attempt_not_submitted`), re-reads the limits for each
   distinct failure, at most once an epoch, and, if they now refuse the row,
   retires it
   (`burn_row_limits_changed`, `alert = true`) and ends the run
   `tempo_deferred:<code>` (exit 75); the next start defers each tempo
   under that code until the limits allow the row again. The limits and the
   permit must also stay compatible from commit through reveal: the chain
   checks them again when it reveals the commit, one or more epochs later,
   and nothing here re-reads them in between. 601's owner sets
   `MinAllowedWeights`; it must not be raised above 1 while the burn row is
   committed. A row dropped at reveal shows up one reveal period later as
   `weights_mismatch` (exit 3, halted), not as a silent continuation.
   Rolling back to an older rtao build needs the
   [rollback check](#rolling-back-past-this-rtao-pin).
10. Without `--dispatch` (the dry run): the journal is not written. Compute
    the operation ID and report whether the journal already retains it
    (`retained`), read the weight preflight at the best block, require that
    rtao's conform step leaves the derived row unchanged (`row_not_preserved`
    otherwise), encrypt it to the predicted reveal round, compose
    `commit_timelocked_mechanism_weights`, type-check it by name against live
    metadata, prepare the unsigned extrinsic for the public hotkey, and print
    its signing-payload hash. No key is read. A weights version key below
    `WeightsVersionKey(netuid)` refuses the dry run
    (`weights_version_key_refused`), as rtao refuses it before signing. The
    report carries the inputs of the chain's checks at commit and reveal:
    `required_version_key`, `validator_permit` and `null_consensus` from the
    preflight, and `max_allowed_validators`, `stake_threshold` and the
    hotkey's own alpha (`own_alpha_stake`, context only: the chain compares
    its stake weight, which also counts inherited alpha and weighted TAO),
    read by name.
11. With `--dispatch`: enqueue the row as `WeightWork` (manifest digest,
    cohort label, revision, mechanism). The operation ID excludes the row, so
    a restart re-derives the same row and joins the retained operation, a
    changed row under the same identity is refused, and a second operation
    for the same mechanism is refused while one is unsettled. For
    `reward-v1`, retained work is settled first: an operation of this target
    and tempo with another row (a re-derived snapshot) that never landed is
    retired and the next label is used, and every other pending operation of
    the mechanism (a superseded target, a missed tempo) that never landed is
    retired. Retirement is journaled (`slop_ninja_retired` with its reason,
    and rtao's hand-off mark) and applies only to an operation with no
    unresolved attempt and no finalized success; anything else defers the
    run (`pending_operation_unsettled`, exit 75) until recovery settles it.
    Then
    authorize the verified operation and row for the
    signing guard, with the owner-row policy chosen for it (the guard
    refuses an intent journaled under another policy,
    `slop_ninja_owner_rows_mismatch`, and a `Submit` intent whose row pays
    any UID but the owner UID recorded with the authorization,
    `slop_ninja_owner_rows_unexpected`), unlock `[dispatch] hotkey_file`, take one metagraph
    snapshot, connect the signing transport (when configured) last, and for
    each finalized block run `dispatch_finalized` (with bounded recovery when an attempt
    needs reconciliation). Once the commit is finalized it reads back
    `TimelockedWeightCommits(netuid, commit_epoch)` for its `reveal_round`.
    Every completed reveal is compared with the row of the operation that
    committed it, read at the block that observed the reveal
    ([Reveal checks](#reveal-checks)). With `[dispatch] max_finalized_blocks`
    unset the run is continuous: it re-commits the target once per tempo
    until a signal or a check. **No authorization expires**: a bundle keeps
    yielding re-commits for as long as its target is the held one, whatever
    time has passed, with or without a coordinator. A fetched newer
    authorization for the same target (issued later) replaces the one each
    re-commit is derived under, in place. A fetched successor is installed at its eligible boundary and the
    run continues on it in the same process (`continuing_on_successor`): the
    recovered journal and reveal watcher carry over, so no restart, recovery
    replay or restart delay separates the old target's last commit from the
    successor's first. At each finalized head the run reloads the journaled
    revocations ([Revocations](#revocations)); when one withdraws the
    current authorization, the withdrawn target's queued work loses its
    signing grant (the guard refuses every later attempt with
    `slop_ninja_authorization_revoked`), work that never signed is retired
    (`authorization_revoked`), and new commitments pause
    (`authorization_paused`, `alert = true`) while reconciliation continues,
    until a newer authorization or successor arrives. A tracked operation
    retired before it signed has nothing to read back (`tracked_retired`),
    so the per-tempo re-commit follows it and a refreshed authorization
    resumes commitments at the next epoch. Without `[fetch]` the
    run first stays until this target's outstanding reveals have been
    compared, then waits as in step 9 and reads the bundle file again each
    epoch.

Before each signature the guard requires that the operation and exact row
were verified in this process (and that no revocation withdrew its target
since), that the call is a timelocked commit (direct weights are never a
fallback), and that commit/reveal is enabled at fresh best-block state. The
adapter's live-surface check runs first.

## Revocations

An authorization stops yielding commitments only when a newer one
supersedes it or a signed `RewardIntentRevocation` withdraws it. A
revocation withdraws every authorization of its target issued at or before
its own epoch; a later authorization of the same target re-authorizes it.
This validator journals each revocation it accepts in
`<journal>.revocations.json` (mode 600, only ever added, read-modify-write
under `<journal>.revocations.json.lock`) after checking its signature under
the configured authorization key and this chain, and checks every per-tempo
commitment against all of them. Revocations arrive two ways:

- `slop_ninja_validator --config ... revoke <file>`: the signed record's
  bytes, as `reward_v1_verify revoke` writes them. Safe while the unit runs;
  the run applies it at its next finalized head.
- the fetcher: every entry of the revocation set that
  `v1/authorizations/current.json` names that the store does not hold yet is
  checked and journaled before anything else in the session, in one locked
  read and at most one write per session (`revocation_recorded`,
  `alert = true`, for each new one), so a validator that missed earlier
  revocations journals them all; entries already journaled are not verified
  again, and an entry that does not verify is skipped and not journaled
  (`revocation_skipped`).

The signature is checked once, when an entry is journaled; loading the store
does not check it again. A revocation can only withdraw authorizations of
its own chain and target, so a trust root rotated since an entry was
journaled never stops commitments of other targets.

An entry whose bytes no longer decode to its recorded digest (a hand edit or
disk damage; the validator's own writes are atomic) is alerted once
(`revocation_entry_unreadable`, `alert = true`) and still applied from its
recorded target and epoch. That withdraws a target with no verifiable
signature behind it, deliberately: the entry was verified when this
validator journaled it, and for an entry that can no longer be read the
safe direction is not committing its target rather than resuming
commitments a revocation may have stopped. It withdraws only its own target
at or before its epoch; restore the entry, or remove it by hand, to clear it.

A store that cannot be read as a whole (unparseable, or another schema):
- while running, the revocations already loaded stay in force and the error
  is alerted once per distinct error (`revocation_store_unreadable`,
  `alert = true`); successors are still checked against them;
- at startup, with nothing loaded yet, the validator alerts
  (`revocation_store_unreadable`, `kept = 0`) and exits with an error rather
  than commit on a check that did not run; systemd restarts it, alerting
  each time, until the store is restored or fixed.

Any refusal leaves the journal paused, and the journal is paused again before
exit, so the next run recovers first.

| Exit | Meaning | systemd |
| --- | --- | --- |
| 0 | Completed (dry run printed; dispatch revealed under `once`; a bounded run reached `max_finalized_blocks` or installed a successor, `superseded`), or interrupted by a signal | dispatch unit restarts (`Restart=always`); dry-run unit stays stopped |
| 1 | Error, such as an RPC failure | restarts |
| 2 | Invalid configuration, missing journal, or `--dispatch` without `[package]` and `[dispatch]` | stays stopped |
| 3 | Halted by a check: subnet or hotkey not registered, commit/reveal disabled, chain mismatch, recovery that needs review, a refused package or row, or chain weights that differ from the row of the operation that committed them | stays stopped |
| 75 | Recovery still in progress after `recovery.max_rounds`, refused upstream (`upstream_storage_refused`), work awaiting recovery (`pending_operation_unsettled`); and, outside a continuous dispatch run, a deferred `reward-v1` tempo (`row_deferred`, `row_would_be_held`, `burn_row_min_allowed_weights`, `burn_row_weights_limit`, `burn_row_no_permit`, `authorization_refused`, a dispatch run ending `tempo_deferred:*`) or no bundle installed (`bundle_unavailable`) | restarts |

## Configuration

Start from [`validator.example.toml`](validator.example.toml). Every chain value
is explicit, and no netuid is built in; `netuid = 0` is refused. Paths must be
absolute. Outside `--dispatch`, only the hotkey's `<hotkey>pub.txt` companion
is read; a missing companion is an error.

`[package]` names the bundle path and the three application public keys
(`package`, `receipt`, `authorization`, 32-byte hex, all distinct). They are
not chain keys. `[fetch]` (requires `[package]`) names the public status
host's `base_url` (`https://`, or `http://` to a loopback host; no
credentials, query or fragment; or `file:///<dir>/`, a local mirror
directory of the same layout, read with every check the host gets), `poll_secs` (300, at least 60, the index
documents' cache lifetime), `stall_secs` (60) and an optional absolute
`cache_dir` (default `public-cache` beside the journal). `[dispatch]` names
`hotkey_file`, the keyfile `--dispatch` signs with; its `pub.txt` must name
the `[wallet]` hotkey. `max_finalized_blocks` unset (the default) makes a
dispatch run continuous; set, it bounds a drill run. `epoch_offset_blocks`
replaces rtao's per-hotkey commit jitter with a fixed offset after each epoch
boundary: on a rate-limited public endpoint a late offset keeps rtao's reveal
replay within a tempo. The replay costs about 14 requests per block, about
one block a minute at 15 requests a minute: at offset 320 of 361 blocks it
replays 25 to 45 blocks, about 23 to 42 minutes; at offset 20 about 340
blocks, several hours (see [TESTNET-M2.md](TESTNET-M2.md)). It must be at
most the subnet's `Tempo`, read live; a larger value never falls due and is
logged as `epoch_offset_beyond_tempo` (`alert = true`).

RPC endpoints must be `wss://` with no credentials, path, query or fragment.
The process sends the configured `User-Agent` and shares one request budget
(`max_requests_per_minute`, 15 by default). Public pools return 429 at about
25 requests per minute from one address. Optional
`signing_requests_per_minute` gives rtao's executor a second connection with
its own budget, which is also its burst: rtao's admission window, from nonce
lease to pool admission, made 26 to 28 calls on testnet at rtao `b5553a76`,
and each commit's minute reached more than about 20 calls on public RPC. The
second connection makes about 16 calls at once, so it opens after every
startup read, with `signing_connect_quiet_secs` (60 by default) on each side
in which the read session makes no call. The admission window itself is
sized from the signing budget ([Paced endpoints](#paced-endpoints)). Until rtao-rpc serves testnet state,
the example lists the public OpenTensor testnet endpoints (testnet shortcut
T3, removed by upstream item R-1).

### Paced endpoints

rtao `0c128160` (D-094 to D-097) changes how a validator that drives rtao's
scheduler from its own loop behaves on an endpoint paced to a few requests a
minute. The validator uses it this way:

- **Admission window.** rtao's window for each weight transaction (signer
  resolution, nonce lease, preparation, signing and the pool's
  acknowledgment) is configurable, 30 s by default, and rate-limiter waits
  count toward it. The validator sets it from the budget of the transport
  that signs (`signing_requests_per_minute`, else `max_requests_per_minute`):
  the 28 calls measured in the window on testnet at rtao `b5553a76`, plus an
  allowance of 20 for the calls this pin can add inside it that the 28 do not
  count (signer resolution, the timelock clock anchor, the commit-reveal
  tag's runtime code and a by-name fallback, one each, and one reconnect of
  about 16 calls), plus one interval for the pool's acknowledgment, paced
  from an empty token bucket, and never under 30 s: 147 s at 20 a minute,
  227 s at 13, 49 s at 60. Both testnet windows fit in the about 492 s
  between a 320-block offset and the end of a 361-block epoch. The first
  live burn-row commit at this pin (2026-11-03) made 21 calls in the
  window, in 3.0 s and with no reconnect, so the 28 cover it and the
  allowance stays for the paths it did not take
  ([TESTNET-M2.md](TESTNET-M2.md#8-measured-limits-on-public-rpc)). A window
  that proves too short only delays the commit: an attempt that signed
  nothing is reported `Failed` and retried in the same epoch, which the
  dispatch loop alerts on (below). The wait for inclusion is a whole
  128-block era after the window, so a longer window never shortens it; a
  commit that lands in the next epoch is followed there by the reveal
  watcher.
- **Unsigned failures.** An attempt that provably signed nothing, such as an
  admission timeout, gives its epoch's cadence slot back and retries after 1
  block, doubling to 32 while such failures repeat. rtao reports it as
  `Failed`, so the validator does nothing to resolve it. rtao's scheduler
  returns `Ok` whatever it recorded, so after each dispatch the loop reads
  rtao's last outcome for the mechanism while the tracked operation waits to
  be signed, and logs `weight_attempt_not_submitted` with `alert = true` once
  per distinct outcome kind (`failed`, `refused:<check>`, `hold_previous`,
  `paused_surface`, `skipped`), resetting when the work signs. It counts
  unsigned failures (`unsigned_failures` in the run report), and when the
  tracked operation has no new journal attempt two epochs (`Tempo + 1`
  blocks each) after it was first seen waiting, by which time at least one
  commit window has passed, it logs `commit_window_passed_without_attempt`
  (`alert = true`; `windows_without_attempt` in the report).
- **Refused reconnects.** rtao retries an HTTP 403/429 refusal of a
  reconnect within the session, after the longer of its `Retry-After` (at
  most 10 minutes) and a backoff doubling from 1 s to 60 s. Both sessions
  set `retry_forever`, so neither a refusal nor a run of endpoint losses ends
  a session; the validator no longer restarts on a refused reconnect. A
  JSON-RPC policy error or a policy close (1008) still ends a session, and
  the dispatch loop then exits (`transport_failed`, exit 1) so the unit
  reconnects; nothing is lost, because the journal is the state.
- **Finalized heads.** The dispatch loop follows only the newest finalized
  head (`Backfill::Latest`), with no request for a skipped height. It needs
  one head in each epoch's commit window, not every height: the
  once-per-epoch cadence is ready at any head at or past the offset in an
  epoch after the last attempt, and the reveal watcher and recovery follow
  finalized history from their own cursors. Replaying a gap first
  (`Complete` or `Bounded(n)`) would only delay the newest head: after an
  hour-long call on testnet, about 400 heights at two requests each.
- **Timelock clock.** rtao anchors its clock from `Timestamp.Now` at each
  commit's preflight block. The validator still observes the best head's
  `Timestamp.Now`, read by name, on every iteration (about two requests),
  since rtao's own read has no by-name fallback.

The metagraph worker is still stopped after its startup snapshot. rtao's
sync now makes no request until a refresh is due, so running it again is
possible but not yet qualified here.

Logs are structured JSON on stderr, or text if configured. Events carry fixed
names and counts. They omit addresses, paths, ciphertext, signed bytes and
endpoint paths. The dry-run and dispatch reports on stdout contain the row
entries, which are public (any validator can derive them from the package).
Dependency crates log at `warn` and above.

## Fetcher

`fetch.rs` reads the publisher's layout `v1`
([`docs/testnet/public-layout.md`](../../../docs/testnet/public-layout.md)):
`v1/authorizations/current.json` names the current
authorization, any renew-by hint refresh and the revocation set as
content-addressed objects; the cohort index
names each cohort's target and whether its receipts are retained; each
cohort's `publication.json` and `chunk-map.json` list every object's SHA-256
and length. These documents are unsigned and only locate objects: every
object must match its listed digest and length, and the assembled bundle is
then verified by `package_v2::verify` against the held target, like an
installed file.

**Where it reads from.** The coordinator signs each closed cohort's
authorization itself and writes the private pointer and objects
(`authorizations/current.json` and `authorizations/<sha256>.signed` in the
receipts bucket, rewritten whenever they differ from its installed bundle and
revocations), and the publisher copies them and the cohort's records to
`v1/`, so a new target reaches a validator with `[fetch]` with no manual
step. The public host is not deployed on testnet yet: until it is, a
validator pointed at it reports `unpublished` and keeps its installed
bundle, so on testnet every validator (the operator's included) still
installs each new target's bundle by hand. A validator may instead read a
local copy of the layout (`base_url = "file:///<mirror dir>/"`). Nothing on
testnet fills such a copy today: a copy for the operator's validator before
the host is public awaits the user's decision (coordinator TESTNET.md,
"Automatic target handoff"). A
`file://` base must name a local absolute directory (no host, query or
fragment); each path is a file under it, and every object is checked as
from the host (listed digest and length, then signatures), so a mirror can
make a fetch fail but cannot change what is verified. Whatever fills the
directory must keep the host's write order so a session never meets a
half-written copy: each file written to a temporary name in the same
directory and renamed into place, every object before the document that
lists it, each cohort's `publication.json` before `v1/cohorts/index.json`,
and `v1/authorizations/current.json` last, and a file removed only once no
document names it. A session that still meets a partial copy reports it
`incomplete` (an object's digest or length differs from its listing) and
the next poll fetches again. The pointer, as the layout defines it:

```json
{
  "schema": "slop_ninja.public.authorization/v2",
  "chain": { "genesis": "<64 hex>", "netuid": 601, "mechanism": 0 },
  "kind": "cohort",
  "authorization": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": 309 },
  "renewal": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": 196 },
  "revocations": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> }
}
```

`kind` is `launch` (a `LaunchAuthorization`) or `cohort` (a
`RewardIntentAuthorization`); `renewal` is a refresh of that
authorization's renew-by hint, or absent; `revocations` is the revocation
set, or absent: the latest `RewardIntentRevocation` of every revoked target
the coordinator holds, concatenated, its `bytes` a whole multiple of one
revocation's signed length. A validator built for schema v1 refuses a v2
pointer by its schema and cannot see revocations. Each object is
the coordinator's signed bytes, named by its SHA-256, and its `bytes` must be
the record's exact signed length (fixed by the schema,
`fetch::authorization_lengths` and `fetch::revocation_length`: 230 bytes for
a launch authorization, 309 for a reward-intent authorization, 196 for a
renewal). The pointer is rewritten in place (`max-age=60`); the objects are
never deleted. Neither the pointer nor the authorization has a validity
window: a pointer that stops changing keeps validators committing its
authorization.

A session reads at most `SESSION_BYTES` into memory for documents and
objects: `MAX_BUNDLE_BYTES` (64 MiB, the bundle file bound), counting an
object read from the cache as well as one fetched. The signed receipt
indexes of the target's cohort and its carrier count against their own
allowance instead (`fetch::receipt_index_allowance`: two indexes of the
largest size the protocol can sign, 972,591 entries or 67,108,947 bytes
each, so 134,217,894 bytes), because they are read into memory in either
form and a cohort may need up to one full index. When every listed object
of the target's cohort and carrier fits the bundle hex-encoded (half the
bound), the bundle embeds them (schema v2, `slop_ninja.reward_v1_bundle/v1`). Otherwise it embeds the
records and names each cohort's signed receipt index and receipt bundles by
their paths in the cache, relative to the cache root, with SHA-256 and
length (schema v3, below); those bundles are streamed to the cache and
never read into the session's memory. The listed lengths leave out the
bundle's own framing (keys, the authorization and its renewal), so a bundle
that exceeds the file bound once assembled embedded is reassembled by
reference from the objects the session already verified. Before reading any
object the session plans what it will read from the listed (or, for
receipts deleted from the host, cached) lengths and refuses a cohort whose
signed receipt index exceeds the largest signable one or whose indexes
exceed the allowance (`receipts_too_large`), or whose listed records could
not fit the file bound or the read bound (`bundle_too_large`); both are
alerting refusals that every session repeats until the host changes. Authorization and renewal
objects are read into memory, their length checked against the schema and
their signature verified before anything else is fetched; they are never
cached. Before reading any receipt bundle, the signed receipt index is
opened, the chunk map's receipt root and count must be the index's, the
bundles' total length must equal what the index's entries declare (a 4-byte
length prefix plus each receipt object's signed length), and every bundle
must be listed in `publication.json` under `receipts/` with the chunk map's
digest and length (`chunk_map_mismatch` or `object_unlisted` otherwise).

**Bundle schema v3.** Embedded, every receipt takes 448 bytes of the
bundle (its chunk framing, object and index entry, hex-encoded; exact,
`target_bundle::receipt_bundle_bytes`), so a schema v2 bundle holds at most
about 150,000 receipts across a target's cohort and its carrier. A daily
cohort records up to 2,224 receipts per entrant per attempt (1,000 cases,
the 1,024-document probe and a 200-case audit draw) plus the references'
batches, so 40 entrants at three attempts need about 545,000 across both
cohorts. A `slop_ninja.reward_v1_bundle/v3` bundle therefore names them
instead: `receipts` and `carrier_receipts` each hold an `index` and a list
of `chunks`, each an object reference `{ "path", "sha256", "bytes" }`; the
embedded `receipt_index` fields and chunk lists stay empty. A path is
absolute, or relative to the validator's fetch cache (only plain
components; the fetcher's form, resolved at every verification against the
configured `[fetch]` cache, so a cache moved together with its
configuration keeps the bundle valid). The validator opens each referenced
file once, checks its length, reads it through the same handle never past
that length, and checks its SHA-256, then verifies the receipts one chunk at
a time against the signed index (each entry exactly once). A cohort's
receipts are therefore bounded by the largest receipt index the protocol
can sign (972,591 entries: the 64 MiB signed-record limit at 69 bytes an
entry, tighter than the codec's 2^20 sequence bound), which is also the
coordinator's template bound, not by the file bound. A missing or
unreadable file is `referenced_object_missing`, a file of another length or
digest `referenced_object_mismatch`; a relative path without a fetch cache,
one that leaves it, or receipts in the other schema's form
`bundle_malformed`. With `[fetch]` configured, the two referenced-file codes
are not a refused package: the run logs `bundle_cache_unreadable` (alerting),
defers (exit 75, or a wait in a continuous run) and asks the fetcher for a
full session, which rebuilds the files while the host retains the receipts
(else `receipts_expired`, and the deferral repeats until a later
authorization or a successor target is installed). Without `[fetch]` (an operator-installed bundle with absolute
paths) they halt as before (exit 3). The operator's `reward_v1_verify`
writes `reward-v1-bundle-v3.json` with absolute paths into the cohort
directory it read, which must stay in place. A validator built before
schema v3 refuses a v3 bundle (`bundle_schema`) and keeps its target.

**What the fetch cache keeps.** Each verification of the installed bundle
(at every start and every switch) reads a v3 bundle's receipts from the
cache files it names. The cache holds detailed receipts only for the
cohorts of bundles this validator holds: every session prunes it to the
cohorts the session named plus its keep set, read when the session prunes:
the cohorts of the installed bundle, of a bundle staged on disk for an
interrupted install (the next start installs or discards it), of a dry
run's bundle staged in memory, and of every verified bundle the process
still holds before installing it (a successor waiting for in-flight work,
the dispatch loop's newest, one in the fetcher's channel; each keeps its
cohorts while it lives). A cohort leaves the cache at the first session
after no held bundle names it, so a successor's install releases its
predecessor's receipts. This is the same retention a schema v2 bundle
always had, which embeds its receipts in the installed file. When the
installed or staged bundle file cannot be read the keep set is unknown
(`installed_bundle_unreadable`, alerting): no session prunes, and none
takes a new cohort into the cache (`cache_keep_unknown`, retryable) until
the file reads again; a file whose manifests do not parse keeps nothing
(`bundle_cohorts_unparsed`, alerting), since it cannot verify. Whether a
validator may keep committing a carried target from its own verified copy
after the host deleted the carrier's receipts (N+2) awaits the user's ruling
on the receipts rule (the same open point as the coordinator's carrier
hold); until then it does, for as long as that target stays installed. A
validator without that copy (a fresh start, or a lost cache) cannot rebuild
the carrier's receipts once the host deleted them: it reports
`receipts_expired` and commits nothing for that target until a later
authorization (naming retained receipts) or a successor is installed;
commitments already submitted still recover from the journal. The cache is not an archive and needs no backup of its own; if
you restore the journal and installed bundle from a backup, a cache from
the same point keeps a carried target committing past the carrier's
deletion. Move the cache only together with its `[fetch] cache_dir`
setting.

Each session holds the cache lock (`<cache>/.lock`; another process gets
`cache_busy`, and `fetch` then exits 75), never follows a redirect, sends no
credential, abandons a request idle for `stall_secs`, and ends any request
and the whole session after `poll_secs`. Verified objects stay in the cache
(directories mode 700, files 600); a partial object is kept as
`<name>.part` and continued with a range request (a host that ignores the
range restarts it); a digest mismatch deletes it. Every session that
located a cohort then prunes the cache to the cohorts it named and its keep
set, whatever its outcome, so a host that lists junk cannot grow it past one
session's reads and the held bundles' cohorts. A session whose
documents and held target are byte-identical to those of the last session
that produced a bundle reports `unchanged` and reads no object, as long as
that bundle's referenced files are all present at their lengths; when one is
gone, or a verification found one changed, the session assembles the bundle
in full (`fetch_rebuild`); signature
checks and bundle verification run on a blocking thread, off the runtime
that drives the finalized-head stream and signing.

Receipts deleted under the N+2 rule are served only from the cache
(`receipts_expired` otherwise; the installed bundle stays). A carrying
cohort produces no target, so its receipts are deleted two cohorts later
even while the current authorization names it: a running validator keeps
them in its cache, but one that starts fresh after that cannot assemble the
carrier's bundle. A validator holding an older target walks the predecessor
chain and checks each intermediate target's records and closure without
receipts (`fetch::advance`), since it commits nothing for them.

`supersede.rs` decides what a verified bundle does: the same bytes keep the
installed one; the same target under a newer authorization (issued later),
or one that yields an operation at the finalized epoch where the installed
one does not (a re-authorization after a revocation), is a refresh (installed,
and swapped into a continuous run in place); a target that activates no
later than the installed one's is kept out (`regression`: a stale or
rolled-back pointer); a later target is a switch at the eligible boundary
(the finalized epoch at or after its activation), which waits while any
pending operation has an unsettled attempt, queues no new commitment of the
old target meanwhile, retires never-signed work as `superseded`, moves the
held record past checked intermediates, installs the bundle and records the
successor's activation. When intermediates are checked, the bundle is first
staged beside the installed one (`<package path>.next`): a crash before the
install completes leaves either the old record and bundle (the next start
discards the staged copy) or the moved record and a staged successor that
the next start installs (`staged_install`), never a bundle older than the
held record. A continuous run then continues on the successor in
the same process. A successor still partial on the host never produces a
bundle, so the old target keeps committing. In a dispatch run the fetcher
polls every `poll_secs`; a startup fetch that must wait hands its successor
to the loop, which starts waiting at once. A refusal (`Refused`, a digest
failure, or documents that disagree with the signed records) is logged as
`fetch_refused` with `alert = true`; outcomes that are only incomplete are
logged as `fetch_kept`. The last outcome code is the dispatch report's
`last_fetch`. `fetch` runs one session without the journal or chain,
installs nothing and prints a content-free report (exit 0 verified, 75
incomplete or cache busy, 3 refused).

Fetched bundles are written in bundle schema v2, or v3 when the receipts
do not fit embedded; in both the `shortcut` field must read `T12`: the label names the bundle format the operator tool also
writes, not how the bundle arrived, and changing it would need a new bundle
schema for every reader.

### Reveal checks

`reveals.rs` keeps `<journal>.reveal-rows.json` (mode 600): the committed
row of every operation enqueued and not yet revealed, with the row hash its
journal correlation records. Each completed reveal in rtao's watcher is
attributed by its commit epoch: an entry learns its commit epoch from its
operation's finalized inclusion block, through rtao's outstanding reveal for
that block or, once revealed, `SubnetEpochIndex` at that block. `Weights`
is read at the block that observed the reveal (at the current block for the
newest reveal when the endpoint no longer holds that state) and compared
with that operation's own row, whichever process or target committed it:
the old target's last commit after a switch, or a commit made before a
restart. A mismatch halts (exit 3). A reveal no entry accounts for (a
commit made by an earlier binary without the ledger) is logged as
`reveal_observed` and never compared with another target's row; a missed
reveal is logged as `reveal_missed` and a reveal whose state cannot be
read as `reveal_unverifiable`, both with `alert = true`. Outcomes are
matched by key, so a full 256-entry ring still compares every new one. An
entry leaves the ledger when its reveal is compared or missed, when the
journal retires its operation, or when a later reveal completed without
one for it (`reveal_unobserved`, `alert = true`).

### Trust roots and restarted chain records

`<journal>.held-target.json` names the authority it was written under (a
digest of the three trusted keys). A record of another authority (rotated
trust roots, decision D20) is set aside as
`<journal>.held-target.json.authority-<prefix>` with `held_target_authority_changed`
(`alert = true`), and the validator starts again from the launch burn row,
so the new authority's launch bundle is accepted. A record written before
records named an authority is bound to the current keys at the next start.

A chain record restarted under the same keys (the announce-time restart
with R0 as R_0 and no predecessor) needs an explicit reset, because the
held target would otherwise refuse the restarted launch bundle and every
target not descended from it:

```sh
systemctl --user stop slop_ninja_validator-dispatch@<name>
slop_ninja_validator --config <config> reset-target
systemctl --user start slop_ninja_validator-dispatch@<name>
```

`reset-target` holds the journal's lock (it refuses while a run holds the
journal), renames the held-target record and the installed bundle (and a
staged `.next` copy) with a `.reset-<unix seconds>` suffix and leaves the journal, its retained work and
the reveal ledger alone; the next dispatch run retires the old target's
never-signed work as superseded. The next run needs the restarted chain's
launch bundle, fetched or installed by the operator.

### Rolling back past this rtao pin

A build at rtao `b5553a76` (the previous pin) decodes three things in the
journal strictly, and refuses the whole weight scope when it meets a field
this pin added:

- the effects of every weight operation rtao has not handed back: `Submit`
  work carries `"owner_rows":"submit"`. In steady state the next tempo's burn
  row is almost always queued and not yet attempted, waiting for its epoch's
  offset, so this is the usual case;
- the attempt material of such an operation (`followed_cr` in a commit that
  followed a non-native commit-reveal tag, `call_encoder` from an
  application call encoder, which this validator does not install);
- the weight checkpoint's reveal watcher: `followed_cr` in a pending reveal
  and `revealed_code`, which rtao keeps as evidence once a reveal completes,
  or is missed, under a non-native commit-reveal tag.

Handed-back and retired operations are never decoded there, so finished work
blocks nothing; the slot's `unsigned_retry` is accepted there. To roll back:

```sh
systemctl --user stop slop_ninja_validator-dispatch@<name>
slop_ninja_validator --config <config> rollback-check --retire
# exit 0: install the older build, then start the unit
```

`rollback-check` holds the journal's lock (it refuses while a run holds the
journal), reads no chain and loads no key. With `--retire` it first retires
the blocking work that never signed (`rollback_prepared`), through the same
journaled retirement as superseded work. It prints the remaining blockers and
exits 0 when none remain, else 3. Work with a signed attempt awaiting
recovery, or a landed commit not yet handed back, settles only by running
this build: start it again and check later. Checkpoint state from a followed
non-native tag never clears (`revealed_code` is retained evidence), so after
rtao has followed one the rollback is roll-forward only. 601 uses the native
tag 4 today. A validator of this build started again queues the retired
tempo's work afresh under the next label, because a retired label is never
joined. From then on each label is one epoch ahead of the epoch it commits
in. Nothing is lost or committed twice. The same shift follows any retirement of a label's never-signed
work after which the validator restarts in the same epoch
(`owner_row_policy_changed`, `burn_row_limits_changed`).

## Fixture packages (T12)

```sh
slop_ninja_fixture_package keygen --dir <0700 directory>    # seeds (0600), prints trusted keys
slop_ninja_fixture_package public-keys --dir <directory>
slop_ninja_fixture_package build --netuid <n> --cohort <c> --opened-block <block> \
  --burn 0:<owner hotkey>:<block> --miner <uid>:<hotkey>:<block> [--miner ...] \
  --keys <directory> --out <bundle.json>
```

The cohort is synthetic plumbing: four tasks in two groups, a baseline that
predicts 0.5, and allow-listed miners that predict closer to the labels, so
every listed miner earns credit. Receipt salts are derived from the attempt
ID so bundles are reproducible; production salts are private. `build` checks
the bundle with the validator's own verification before writing it. No
command reads a chain key or prints a seed.

## Install

Build from source as in the [validator guide](../../../docs/testnet/validator-guide.md#2-install).

Install the binaries, the units, the configuration and lingering for
instance `<name>`, check the configuration, create the journal and enable
the continuous dispatch unit (`--dry-run` prints each step instead; Linux
only):

```sh
subnet/crates/validator/deploy/install.sh --instance <name> --from <target>/release \
  --config config.toml --init --enable --start
```

The installer requires `SHA256SUMS` in the source directory to list each of
the two binaries exactly once and both to match it (`--unverified` installs
without `SHA256SUMS`, for binaries you built yourself). `SHA256SUMS` only
shows the download is intact: before running the installer, verify the
release's detached signature `SHA256SUMS.sig` with `ssh-keygen -Y verify` and
the release key on the testnet landing page (`docs/testnet/README.md`,
section "Release key"). It installs the configuration with mode 0600 only
when none exists, runs
`check-config`, creates the journal with `--init` (`init` refuses an existing
path, so a typo never replaces history), enables lingering
(`loginctl enable-linger`), reloads the user manager and enables
`slop_ninja_validator-dispatch@<name>`. Back up the journal with the wallet.
It holds sensitive commit material, so do not publish it. Back up the fetch
cache, the installed bundle and the held-target record with it: a v3
bundle's receipts live only in the cache once the host deleted them (see
"The fetch cache is validator state" above). A restart, reinstall or update
must keep all four in place (or move the cache only with its configuration).

`slop_ninja_validator-dispatch@<name>` runs `run --dispatch` with
`Restart=always` and `WantedBy=default.target`, so with lingering it starts
at boot and restarts after every exit except 2 and 3, after a fixed 60 s.
(systemd's `RestartSteps` backoff never resets while a unit keeps being
restarted automatically, whatever the exit code, so it would leave every
later restart at its maximum.) A continuous run does not exit for routine
work: it continues on a successor in the same process and waits out a
deferred tempo. After exit 2 or 3, fix the cause, then
`systemctl --user reset-failed slop_ninja_validator-dispatch@<name>` and
start the unit again. `slop_ninja_validator@<name>` runs the dry run with
`Restart=on-failure`. Both set `MemoryMax=2G` and `MemorySwapMax=0`, twice
the measured worst case rounded up to a whole GiB. The worst case is at the
receipt index bound (`fetch_tests::peak_memory_at_the_receipt_index_bound`,
ignored by default; debug build, 2026-11-03): a target's cohort and its
carrier each with the largest receipt index the protocol can sign (972,591
receipts, a 67,108,947-byte index, so a 35,773-byte v3 bundle verifying
1,945,182 receipts). One fetch session peaked 409 MiB above its starting set,
one verification 543 MiB (with that session's bundle still held), and the
most a continuous run holds at once (the installed verification, the loop's
newest staged bundle, one waiting in the fetcher's channel and a session in
flight) 810 MiB; each verified bundle holds its decoded receipt indexes. With
the 63 MiB start measured at the bundle bound that is about 0.85 GiB, too
close to the earlier 1G. The embedded bound measured less
(`fetch_tests::peak_memory_at_the_bundle_bound`, 2026-11-01: a
66,437,318-byte bundle of 148,235 receipts, 217 MiB for one session and 386
MiB for the most a continuous run holds above a 63 MiB start). A live
dispatch run with a 15,344-receipt bundle peaked at 56 MiB; the installed
daily template (272,880 receipts a cohort) reads about 37.7 MB of indexes
for a target and its carrier. A larger receipt or bundle bound needs a new
measurement. Keep
`recommit = "per_tempo"` under the dispatch unit: with `once`, each restart
commits again. Stop the unit before using the offline journal CLI, because
the journal holds a lifetime lock:

```sh
systemctl --user stop slop_ninja_validator-dispatch@<name>
slop-ninja-journal --journal <journal_path> inspect
```

Never delete the journal or its lock to clear a refusal.

### Version and build commit

`slop_ninja_validator --version` and `slop-ninja-journal --version` (or
`-V`) print the crate version and the commit the build came from; include
that line when you report a problem. The subnet crate's build script reads
the commit with `git rev-parse HEAD` and appends `-dirty` when tracked files
had uncommitted changes the last time the script ran (Cargo re-runs it when
HEAD moves, not on every edit). It records no commit when Git or the
repository is unavailable, or when Git cannot name the files that change on
a new commit (it falls back from `--git-path` to `--absolute-git-dir`, Git
2.13 or later). For a build from a source archive, set
`SLOP_NINJA_BUILD_COMMIT` to the full 40-digit commit; any other non-empty
value fails the build. Release builds set it to the tagged commit.

## Fault-injection hooks

`Faults` hooks sit at the journal boundaries: journal opened, each recovery
round, commit/reveal checked, live surface evaluated, resume persisted,
package verified, row derived, row enqueued (dispatch only), and after each
`dispatch_finalized` call. They can be armed only under `cfg(test)`; release
builds compile hooks that always pass. Boundaries inside rtao's submission
(after signing, after send) are covered by rtao's journal, which persists the
signed attempt before network I/O, by the offline lost-response tests below,
and by the live drills in [TESTNET-M2.md](TESTNET-M2.md#7-fault-drills).

## Tests

```sh
cargo test --manifest-path subnet/Cargo.toml --locked -p slop_ninja_validator -- --test-threads=1
cargo clippy --manifest-path subnet/Cargo.toml --locked --workspace --all-targets -- -D warnings
```

The tests are offline. They use fake chain ports, real file journals and
public deterministic dev seeds, and create no client, signer or RPC
connection. Their journals live in per-test directories under
`SLOP_NINJA_TEST_TMPDIR` when set, otherwise under the system temporary
directory. They cover:

- package verification: every tampered envelope, receipt object, missing or
  reordered receipt, cross-cohort record, wrong role, untrusted key, wrong
  subnet or chain, and malformed bundle is refused before a row exists;
- row derivation, including UID reuse (credit to UID 0) and a foreign UID 0
  (no row);
- the signing guard's verified-row, contract, owner-row-policy and
  timelocked-call checks, including the policy of a real journaled `Submit`
  intent;
- restart at every application fault boundary, and a signed attempt whose
  response was lost, each yielding exactly one operation and no new attempt;
- a dry run that leaves the journal unchanged, so a mistaken package never
  blocks the corrected one, and refusal of a row rtao would hold;
- startup classifying an upstream storage refusal during recovery as a
  recorded retry, and recording readiness before recovery so that a
  readiness write never refuses the explicit resume;
- the fetcher against an in-memory public host: a published cohort assembles
  into exactly the bundle the operator tool writes; a second session reads
  only the documents; an interrupted object resumes with a range request
  (and restarts when the host ignores it); a digest mismatch deletes the
  partial file and an over-long declared body is refused before it is read;
  a successor not yet listed, missing an object or without published
  receipts produces no bundle; receipts deleted under N+2 come only from the
  cache; a validator holding the launch row advances through an intermediate
  target without its receipts, a tampered intermediate and a held target
  outside the chain are refused; the launch pointer (committing at any
  epoch past its renew-by hint), a foreign chain, a hint refresh that is not
  a newer authorization and a re-authorization that is; a pointer
  revocation journaled before anything else, withdrawing the staged
  authorization, and a forged one refused unjournaled; and the real HTTP
  transport against a loopback
  server (range continued or ignored, 404, a redirect refused);
- the fetcher's bounds against a hostile host: an authorization of the wrong
  length is never requested and one with a bad signature stops the session
  before anything else is read or cached; a receipt bundle the signed index
  does not account for, another receipt root, or a bundle the publication
  record does not list is refused before any bundle is read; every session,
  refused ones included, prunes the cache to what it named and the installed
  cohorts; an unchanged session reads no object; a trickling HTTP host is
  cut off by the total bound;
- every supersession decision (unchanged, refresh by a newer authorization
  or by coverage, an authorization that is not newer, a regression to an
  earlier target, a successor before
  activation, waiting on in-flight work, a switch retiring never-signed
  work), the install order (intermediate, bundle, successor's record), and
  a stale pointer after a switch refused by the held record and by the
  decision;
- the dispatch loop's fetch decisions through a fake loop: wait without a
  chain read while work is in flight, then switch, then compare the old
  target's last reveal with its own row from the reveal ledger (never the
  successor's), observe an unattributed reveal without halting, and refresh
  a revoked authorization in place with a re-authorization; the ledger itself (operations that never commit,
  a full 256-entry outcome ring, expiry, an unreadable file);
- held-target records bound to the trust roots (another authority set
  aside, an unbound record bound) and a dry run that stages a fetched bundle
  in memory and writes nothing;
- the `[fetch]` and continuous-dispatch configuration checks, the
  installer's dry run and checksum rules, and the dispatch unit's restart
  policy;
- step 0 and payload golden vectors against the recorded testnet spec 486
  metadata ([`fixtures/payload-486.txt`](fixtures/payload-486.txt)); see the
  [runbook](TESTNET-M2.md#0-subnet-601-at-preparation-time);
- every storage key the live ports read by name (`live::keys`, integer keys as
  `Value::Int`) against keys built independently of rtao from subtensor's
  declared hashers, and hand-written SCALE values for each item decoded
  through rtao's by-name decoder and the live value parsers, including
  absent-key defaults.

Regenerate the golden vectors after an intended change with
`SLOP_NINJA_BLESS_PAYLOADS=1`. They are test data, not an accepted-runtime
list; a runtime upgrade that changes an encoding updates them and never stops
a running validator.

On `build-host` (Linux x86_64, Rust 1.99.1), 40 tests passed on 2026-10-30 at
rtao `b5553a76`, and Clippy passed with warnings denied; with the `reward-v1`
bundle path, 45 tests passed on 2026-10-31. The new test drives a launch
bundle: the burn row, a bundle signed under another key refused before any
row, the per-tempo authorization (since 2026-11-03: past its renew-by hint,
then stopped by a journaled revocation and resumed by a later
authorization), and the rtao hold. Cohort bundles are covered by the protocol crate's `target_bundle`
tests and the coordinator's end-to-end simulation, and ran live in the
`reward-v1` rehearsal ([coordinator TESTNET.md](../../coordinator/TESTNET.md)). The default dry run
ran against testnet subnet 601 on 2026-10-30
([recorded in the runbook](TESTNET-M2.md#recorded-dry-run-2026-10-30)); the
dispatch path has not run against a live chain. macOS is untested.

With the fetcher, continuous dispatch, supersession, the reveal ledger and
the installer, 72 tests passed on 2026-11-01 on the development host (Linux
x86_64, Rust 1.99.1), with workspace Clippy and the `testnet-drills` build
clean under denied warnings and the `no_runtime_gates` lint passing. The
tests are offline (an in-memory public host, a loopback HTTP server, the
real file journal, a fake dispatch loop); the dispatch loop's chain reads
themselves have no fake-chain test. With the staged install and the
no-fetcher pause wait, 73 tests passed on 2026-11-02 (the memory
measurement above is the one ignored test). The fetcher has not run against
a live public host, which is not yet deployed (the authorization pointer is
not published), and no continuous run has crossed a cohort boundary on
subnet 601 yet; the persistent unit's live runs are in
[TESTNET-M2.md](TESTNET-M2.md#10-persistent-unit-2026-11-01).

At rtao `0c128160`, 83 tests passed on 2026-11-03 on the development host,
with workspace Clippy and the `testnet-drills` build clean under denied
warnings. New: the burn row queued under `Submit` and recorded in the
journal, burn rows the chain limits refuse deferred before the journal, a
burn row an earlier build queued under `Hold` retired for `Submit`, a
retained burn row joined (signed, or landed and handed back) or retired
(never signed) after the limits changed, cohort targets whose row pays only
the owner UID queued under `Submit` and ones paying this validator's own
UID, alone or with the owner, deferred at the first commitment and at the
per-tempo re-derive, the rollback check finding the queued burn row,
rtao's hold predicate for self-only, self-plus-owner, all-zero and
owner-validator rows, and three [paced-endpoint](#paced-endpoints) tests:
the admission window's derivation, a refused reconnect kept within the
session and a policy close ending it (against rtao-testkit's loopback fake
node), and the newest finalized head after a long iteration (against its
fake chain). No test runs a `Submit` row through rtao's real scheduler with
this validator's guard and reveal ledger; the first live burn-row commit on
601 is that check. A read-only dry run of this build against 601
(2026-11-03, no key, a journal copy) built the launch burn row's commit,
one entry at the owner UID under `Submit`, with the subnet's limits
(`MinAllowedWeights` 1, `MaxWeightsLimit` 65535) read by name. On
2026-11-03 validator2 (UID 2, not the owner), installed from this build,
committed that row timelocked in block 8,362,443 and revealed it at
8,362,485 with the reveal matching the journaled row; the chain then held
UID 2's row as weight 1.0 on the owner UID 0 and nothing for any miner UID.

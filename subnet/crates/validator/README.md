# Native validator

`slop_ninja_validator` is the native SlopNinja validator. It builds on the
journaled adapter in [`subnet/`](../../README.md) and rtao `fa83052d`
(tag `snapshot-2026-11-05.2`). For `reward-v1` it verifies the coordinator's
signed row and the authorization it was committed under with the shared
[protocol crate](../protocol/README.md) and commits exactly that row as a
timelocked weight commit through rtao's journaled scheduler (the copy model:
it never receives a score package or receipt and recomputes nothing). By
default it only prepares the commit and prints its hash; `run --dispatch`
signs.

With `[fetch]`, the validator fetches the coordinator's published records
from the public status host, verifies them and installs the bundle itself
([Fetcher](#fetcher)). Without it, the operator installs one local bundle
file. A `slop_ninja.coordinator_row_bundle/v1` bundle carries the active
`reward-v1` row: the launch or reward-intent authorization (hex), any
renew-by hint refresh, and the coordinator's signed row of it, the three
objects the authorization pointer names (`package_v2::row_bundle`). It is
the only bundle a validator commits from: it verifies the signed row and
commits it, and verifies no receipt and recomputes no score. Every bundle
of records, a score package or receipts is retired (`bundle_schema_retired`):
the earlier `slop_ninja.reward_v1_bundle/v1` and `/v3` bundles (a cohort's
records and receipts, for recomputation) and the T12
`slop_ninja.fixture_package/v1` bundle (a `testnet-reward-v0` score package
with its receipt index and receipts, built by `slop_ninja_fixture_package`).
One still installed when this build starts is a deferral, never a halt and
never a commit. Nothing new commits until a row bundle is installed, while
recovery and the fetcher keep running (a continuous run waits for the
fetcher, a one-shot run exits 75 for a later retry). A row bundle of the
target the validator already holds is installed in place over a retired
one at once, whatever commitment is in flight (it belongs to the same
target), so the next tempo commits without a restart; a successor target
that must wait for in-flight work over a retired bundle exits 75 instead,
and the restart's finalized recovery settles that work. The fixture-package
verifier (`package.rs`) remains only as a signed input of the crate's own
journal and dispatch tests. The live procedure for subnet 601 is
[TESTNET-M2.md](TESTNET-M2.md).

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
   recovery error exits 1. A finalized failed attempt (a weight call the
   chain included but refused) first waits for its receipt to be read: a
   failed read logs the alert `failure_receipt_unreadable` (once per run and
   class, naming only the read's fixed class) and leaves recovery incomplete
   (exit 75, `recovery_incomplete`). A read that cannot clear by itself
   (`state_discarded`, `block_not_found`, `decode`, `metadata`) is recorded
   as `receipt_unavailable` and halts for review instead; each later start
   reads it again. Once
   decoded, an ordinary refusal settles by its error name (rate limit, commit
   too soon, not enough stake) and is logged as the alert
   `weight_commit_refused`; any other failure, or a receipt that contradicts
   the journal, halts recovery with `recovery_requires_review` (exit 3, after
   the alert `finalized_failure_requires_review`) until it is reviewed
   offline with `slop-ninja-journal review-failure`
   ([subnet README](../../README.md#journal-controls)). A running dispatch
   process does the same at each finalized head: it retries an unread
   receipt at the next head, and after a settled refusal commits again at a
   later tempo.
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
7. With a `[package]` configured, verify the bundle. A bundle of a retired
   schema (the `testnet-reward-v0` fixture bundle or a `reward-v1` records
   bundle) is a deferral (`bundle_schema_retired`), never a commit. A
   `reward-v1` coordinator-row bundle goes through the protocol crate's
   `coordinator_row::verify`: the authorization and the coordinator's row
   open under the trusted
   authorization key and name this chain, the row names the authorization's
   digest, target, cohort and issue, and its target is the held one (same
   activation epoch) or one activated later. Nothing of the hidden
   evaluation is read or recomputed. The active target is this validator's
   own: `<journal>.held-target.json` (mode 600) records the target and
   activation a verified row named, and only that target or a later one is
   accepted afterwards; the launch row only until a cohort target has
   activated. Any failure is a refusal (exit 3) before a row exists.
8. Read `SubnetworkN`, `Keys` and `BlockAtRegistration` by name at the same
   finalized block into a registration snapshot. A `reward-v1` row is the
   coordinator's signed entries (`VerifiedRow::derive_row`) with one
   score-free rule, the one the coordinator's own derivation applies: the
   burn entry, and every entry whose `(uid, hotkey, registration block)`
   the snapshot no longer holds (a deregistration, a re-registration, the
   UID handed to another hotkey), go to the UID the finalized
   `SubnetOwnerHotkey` holds (`row_deferred` when units must burn and it
   holds none). A UID that changed hands is never paid to its new holder,
   through coordinator silence included; each is logged
   (`row_uid_moved`). The snapshot also supplies the row's registration
   metadata and the owner-row policy. The
   operation is the `WeightOperationV2` for the current `SubnetEpochIndex`,
   issued only under the authorization checked for that epoch
   (`authorization_refused` otherwise). A `reward-v1` row sums to 65,535; it
   is committed, and compared with `Weights` after its reveal, max-upscaled
   so that its largest entry is 65,535 (`WeightRowV2::chain_values`), the
   form rtao's preparation and the chain's reveal both keep. Each later
   tempo's re-commit takes the same row under the installed bundle and
   checks the authorization again.
9. Read `SubnetOwnerHotkey(netuid)` by name and choose the row's owner-row
   policy (rtao's `OwnerRowPolicy`, D-093). A `reward-v1` row whose every
   positive entry goes to the UID the finalized owner hotkey holds is an
   owner burn row: the launch burn row under the launch authorization, or a
   verified cohort target whose row pays only the owner UID (its units burn,
   or an entrant registered under the owner hotkey is paid there; the chain
   receives the same row either way). It is queued under `Submit`,
   which rtao journals with the work (`"owner_rows":"submit"`) and commits and
   reveals like any row. Every other row is queued under `Hold`, the
   default, whose journaled bytes are unchanged.
   The hold decision is rtao's own predicate, `OwnerRowPolicy::holds`, under
   that policy: this validator's UID is the owner's only when its own hotkey
   is the owner hotkey, and any other UID is the owner's when the finalized
   snapshot maps it to the owner hotkey. A row rtao would still hold (a
   self-only or self-plus-owner row, or an all-zero row) is deferred
   (`row_would_be_held`), so this validator commits nothing that tempo and
   its previous weights stay on chain. rtao decides the hold again from the
   UIDs at the block each attempt is prepared at (D-098). Queued work it
   holds there, because registrations changed after the row was derived
   (the owner hotkey moved to another UID, say), reports `Refused` named
   `OwnerRowHeld` and would block the mechanism at every head. rtao decides
   at the best block, while the next start re-derives from the finalized
   head, so the dispatch loop waits until the finalized head reaches the
   best block of the first refusal (`owner_row_held_waiting_for_finality`)
   and rtao still refuses. It then retires the work through rtao's
   `retire_pending` (reason `owner_row_held`, alert `owner_row_held_retired`)
   and ends the run with `tempo_deferred:row_would_be_held` (exit 75). The
   next start re-derives the tempo against the finalized registrations under
   the next label, or defers it while it would still be held. A hold that
   clears before it is finalized costs no label. rtao journals its retire
   marks and our reason in two updates; work with only rtao's marks
   (`scheduler_retired`, a stop between the two) still counts as retired
   (reason `scheduler_retired`) and is never joined again. Before a burn row is queued, rtao's
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
does not check it again. A readable revocation can only withdraw
authorizations of its own chain and target, so a trust root rotated since an
entry was journaled never stops commitments of other targets (an entry whose
target can no longer be read is the exception below).

An entry whose bytes no longer decode to its recorded digest (a hand edit or
disk damage; the validator's own writes are atomic) is a revocation still in
force (user decision of 2026-11-06). It is alerted once
(`revocation_entry_unreadable`, `alert = true`) and applied from what it
still records:

- its recorded target parses: it withdraws that target's authorizations
  issued at or before its recorded epoch, as a readable entry would (its
  recorded authorization is an audit binding only and need not parse);
- its recorded target does not parse: the target it withdrew cannot be
  recovered, so it withdraws every authorization issued at or before its
  recorded epoch, whatever its target. The installed authorization, and any
  staged successor, commits only if it was issued after that epoch;
- signed bytes that still decode, but not to the recorded digest, are
  applied as well.

That withdraws authorizations with no verifiable signature behind it,
deliberately: the entry was verified when this validator journaled it, and
for an entry that can no longer be read the safe direction is not
committing rather than resuming commitments a revocation may have stopped.
Withdrawn work behaves as under any revocation (`authorization_revoked`):
queued work that never signed is retired and new commitments pause, while
commitments already signed and their reveals stay with recovery. Restore
the entry, or remove it by hand, to clear it.

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
  `paused_surface`, `skipped`), resetting when the work signs. Queued work
  rtao's owner-row guard holds reports `refused:OwnerRowHeld` and is retired
  (step 9); a weight version below the subnet's `WeightsVersionKey` reports
  `refused:IncorrectWeightVersionKey` until the operator raises
  `weight_version`. It counts
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

`fetch.rs` reads one document of the publisher's layout `v1`
([`docs/testnet/public-layout.md`](../../../docs/testnet/public-layout.md)):
`v1/authorizations/current.json`, which names the current authorization, any
renew-by hint refresh, the revocation set and the coordinator's signed row
(`row`) as content-addressed objects. It reads those objects into memory,
checks each against its listed SHA-256 and length, journals the revocation
set first, and assembles a coordinator-row bundle (schema
`slop_ninja.coordinator_row_bundle/v1`) that `package_v2::verify` checks
against the held target, like an installed file. This is the copy model
(user decision of 2026-11-04): the validator commits the coordinator's row
and does nothing else. No cohort record, score package, receipt, receipt
index or chunk map is read (they are coordinator-private), nothing is
recomputed, and nothing is cached on disk. What an earlier layout cached in
the cache directory (`cohorts/`, `authorizations/`: the records and
receipts an installed v3 bundle references, some of them deleted on the
host since) is left untouched, for a rollback (Install below).

**Where it reads from.** The coordinator signs each closed cohort's
authorization itself and, within a cron tick of deriving the row of a
commitment (before its own hotkey signs anything; its weight delivery is
not waited for), its row of that commitment (`CoordinatorRow`, under the
authorization key: chain, cohort and manifest digest, target and
activation, authorization, epoch, owner hotkey, and the `(uid, hotkey,
registration block, weight)` entries). It signs a new record whenever the
paid entries change under the same authorization (a deregistration, a
re-registration, a UID changing hands, an owner move), with a later epoch. It writes the private
pointer and objects (`authorizations/current.json` and
`authorizations/<sha256>.signed` in the receipts bucket); the pointer names
a row only beside the authorization it was committed under. The publisher
copies them to `v1/` as soon as they verify. The public host is not deployed
on testnet yet: until it is, a validator pointed at it reports `unpublished`
and keeps its installed bundle, and an operator with read access to the
receipts bucket can serve its own validator a mirror instead
(`deploy/row-mirror.sh`, below). A validator may instead read a local copy of
the layout (`base_url = "file:///<mirror dir>/"`); a `file://` base must
name a local absolute directory (no host, query or fragment), each path is
a file under it, and every object is checked as from the host, so a mirror
can make a fetch fail but cannot change what is verified. Whatever fills the
directory writes each object before the pointer that names it, the pointer
last, each through a temporary name renamed into place. The pointer:

```json
{
  "schema": "slop_ninja.public.authorization/v2",
  "chain": { "genesis": "<64 hex>", "netuid": 601, "mechanism": 0 },
  "kind": "cohort",
  "authorization": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": 309 },
  "renewal": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": 196 },
  "revocations": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> },
  "row": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> }
}
```

`kind` is `launch` (a `LaunchAuthorization`) or `cohort` (a
`RewardIntentAuthorization`); `renewal` is a refresh of that
authorization's renew-by hint, or absent; `revocations` is the revocation
set, or absent: the latest `RewardIntentRevocation` of every revoked target
the coordinator holds, concatenated, its `bytes` a whole multiple of one
revocation's signed length; `row` is the coordinator's signed row, absent
until the coordinator has derived a row under the authorization (its own
once-a-tempo finalized snapshot, independent of its own commitment; the session then
reports `row_unpublished`, retried, and the installed bundle keeps
committing). An authorization's, renewal's or revocation's `bytes` must be
the record's exact signed length (`fetch::authorization_lengths` and
`fetch::revocation_length`: 230 bytes for a launch authorization, 309 for a
reward-intent authorization, 196 for a renewal), and a row's at most the
protocol's longest row (`fetch::row_bound`: a roster's entrants and one burn
UID); a longer listing is refused before it is requested. Neither the
pointer nor the authorization has a validity window: a pointer that stops
changing keeps validators committing its row.

**What is checked.** The row and the authorization open under the trusted
authorization key and name this chain; the row names the authorization's
record digest, target, carrying cohort and manifest digest, and issue
epoch; a launch row only while the held target is the launch burn row; a
cohort row's target is the held one with the same activation epoch, or one
that activates later (`coordinator_row::verify`). Each per-tempo commitment
checks the authorization again for its epoch and against the journaled
revocations, with no end epoch. The row committed is the record's entries
with the registration rule of step 8 (units of a registration the
validator's own finalized snapshot no longer holds burn through the owner's
UID; `row_uid_moved` is logged); the snapshot also decides the owner-row
policy and the burn-row limits. Nothing else changes a UID or a weight, and
nothing is recomputed from scores. Bundles of cohort records and receipts
(schemas `slop_ninja.reward_v1_bundle/v1` and `/v3`) are retired
(`bundle_schema_retired`, a deferral).

A session reads at most `SESSION_BYTES` (`MAX_BUNDLE_BYTES`, 64 MiB) into
memory. A session whose pointer and held target are byte-identical to those
of the last session that produced a bundle reports `unchanged` without
reading any object. One session at a time holds the cache lock
(`<cache>/.lock`).

`supersede.rs` decides what a verified bundle does: the same bytes keep the
installed one; the same target and authorization with a row of a later
commitment epoch (the coordinator re-signed it because its entries changed)
is a refresh, and one of the same or an earlier epoch is kept out
(`row_not_newer`: a stale or replayed pointer never rolls the row back);
the same target under a newer authorization (issued later),
or one that yields an operation at the finalized epoch where the installed
one does not (a re-authorization after a revocation), is a refresh (installed,
and swapped into a continuous run in place); a target that activates no
later than the installed one's is kept out (`regression`: a stale or
rolled-back pointer); a later target is a switch at the eligible boundary
(the finalized epoch at or after its activation), which waits while any
pending operation has an unsettled attempt, queues no new commitment of the
old target meanwhile, retires never-signed work as `superseded`, installs
the bundle and records the successor's activation. A bundle staged beside
the installed one (`<package path>.next`, by an interrupted install of an
earlier build) is installed at the next start when it verifies against the
held record and discarded otherwise (`staged_install`). A continuous run
then continues on the successor in the same process. In a dispatch run the
fetcher polls every `poll_secs`; a startup fetch that must wait hands its
successor to the loop, which starts waiting at once. In the loop's
background sessions a refusal (`Refused` or a digest failure) is logged as
`fetch_refused` with `alert = true`; outcomes that are only incomplete are
logged as `fetch_kept`. The startup session and the sessions of a wait after
a deferred tempo log their report as `fetch_startup` or `fetch_waiting`
(info level), a refusal there only as `"alert": true` inside the report.
The last outcome code
is the dispatch report's `last_fetch`. `fetch` runs one session without the
journal or chain, installs nothing and prints a content-free report (exit 0
verified, 75 incomplete or cache busy, 3 refused).

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

A build at rtao `0c128160` (the previous pin) decodes three things in the
journal strictly, and refuses the whole weight scope when it meets a field
it does not know: the effects of every weight operation rtao has not handed
back, the attempt material of such an operation, and the weight
checkpoint's reveal watcher. This pin writes the same fields there (`Submit`
work's `"owner_rows":"submit"`, a followed non-native tag's `followed_cr`
and `revealed_code` included) with one exception: `period_changed` in a
pending reveal whose subnet reveal period changed while it was pending
(D-112). It clears when that commitment settles; `rollback-check` lists it
(`checkpoint.reveals.pending.period_changed`), never retires anything for it
and exits 3: run this build until the reveal settles, then check again. Work
rtao retired (`retire_pending`, the `OwnerRowHeld` path) is handed back and
never decoded there. This pin can also raise the journal to schema 4, which
`0c128160` refuses, but only through a compaction, and nothing here
compacts; such a journal is listed (`journal.schema_version`) and is
roll-forward only.

The held-target record (`<journal>.held-target.json`) also blocks once it
is `validator_held_target/v2`, the schema that carries the restart floor
(P1-V). A build before P1-V reads only v1 and refuses a v2 record, so
`rollback-check` lists it (`held_target.schema`), retires nothing and exits
3: roll forward only. A v1 record blocks nothing; this build rewrites it as
v2 at its next record.

One kind of finished work also blocks: a finalized failed weight call. Every
build before `review-failure` (earlier releases at `0c128160`, such as the
2026-11-03 AR-3 install) counts each finalized failure the journal retains,
reviewed, settled or not, halts on it with `recovery_requires_review` (exit
3), and has no way to review it. rtao keeps the attempt as evidence, so it
never clears: `rollback-check` lists it (`attempt.outcome.finalized_failure`,
or `extensions.slopninja.failure-review.v1` once reviewed or settled),
retires nothing, and exits 3. A journal that has held one settled
`CommittingWeightsTooFast` or `NotEnoughStakeToSetWeights` refusal is
roll-forward only, at or after the release that added `review-failure`. To
roll back:

```sh
systemctl --user stop slop_ninja_validator-dispatch@<name>
slop_ninja_validator --config <config> rollback-check --retire
# exit 0: install the older build, then start the unit
```

`rollback-check` holds the journal's lock (it refuses while a run holds the
journal), reads no chain and loads no key. With `--retire` it first retires
any blocking work that never signed (`rollback_prepared`), through the same
journaled retirement as superseded work; at this pin no work-level field
blocks, so it retires nothing. It prints the remaining blockers and the next
step and exits 0 when none remain, else 3. Work with a signed attempt
awaiting recovery, or a landed commit not yet handed back, settles only by
running this build: start it again and check later. A validator restarted
after any retirement of a label's never-signed work in the same epoch
(`owner_row_held`, `owner_row_policy_changed`, `burn_row_limits_changed`)
queues that tempo's work afresh under the next label, because a retired
label is never joined; from then on each label is one epoch ahead of the
epoch it commits in. Nothing is lost or committed twice.

## Fixture packages (T12, retired)

A release validator refuses these bundles (`bundle_schema_retired`, a
deferral, never a commit). The tool remains for the crate's own tests and
for building signed fixture inputs:

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
subnet/crates/validator/deploy/install.sh --instance <name> --from <target>/release --unverified \
  --config config.toml --init --enable --start
```

`--unverified` is for binaries you built yourself. For a signed release, pass
its directory without it: the installer then needs `SHA256SUMS` and the
release's detached signature over it, `SHA256SUMS.sig`. Before installing
anything it copies both and the two binaries to a private temporary
directory, verifies the signature there with `ssh-keygen -Y verify -n file`
(OpenSSH 8.1 or later) against the release key pinned beside it,
`deploy/release-allowed_signers` (the key on the testnet landing page,
`docs/testnet/README.md`, section "Release key"; an `allowed_signers` or
`.pub` file in the release is never read), then requires `SHA256SUMS` to list
each binary exactly once and both copies to match it, runs each copy once
(`--help`) so that a build this host cannot execute (another
architecture, an older C library) replaces nothing, and installs those
copies. Any failure refuses with nothing installed. `--unverified` permits a
directory without `SHA256SUMS.sig`, or without `SHA256SUMS` too, but still
checks any signature or digest list the directory holds. The release
binaries are Linux x86_64 only, the one tested platform. Run the installer
from a repository checkout, never from a copy that came with a release: the
pinned key it trusts is the file beside it, so `install.sh` and
`release-allowed_signers` are not release files.
`deploy/test-install.sh <empty scratch directory>` tests these checks with a
throwaway key and stub units (no chain call, nothing installed outside the
scratch directory). The installer installs the configuration with mode 0600 only
when none exists, runs
`check-config`, creates the journal with `--init` (`init` refuses an existing
path, so a typo never replaces history), enables lingering
(`loginctl enable-linger`), reloads the user manager and enables
`slop_ninja_validator-dispatch@<name>`. Back up the journal with the wallet.
It holds sensitive commit material, so do not publish it. Back up the
installed bundle and the held-target record with it; a row bundle needs
nothing else (it references no cached object). A restart, reinstall or
update must keep them in place.

**Upgrading from a v3-bundle release.** This build never deletes the
earlier release's fetch cache (`cohorts/`, `authorizations/` under the cache
directory): the receipts its installed v3 bundle references live only
there once the host deleted them under N+2, so a rollback to that release
needs it to verify its bundle again. Keep it, and remove it yourself only
once this release is accepted (no rollback planned), with the unit stopped:
`rm -r <cache>/cohorts <cache>/authorizations`. Until a coordinator-row
bundle is installed, the upgraded validator defers (`bundle_schema_retired`)
and commits nothing new; install this release only after the coordinator and
publisher publish rows (`subnet/coordinator/TESTNET.md`, "Public allow-list
and the copy model").

**A row mirror without the public host.** `deploy/row-mirror.sh <mirror
dir>` copies the private authorization pointer and the objects it names
(the authorization, any renewal, the revocation set and the coordinator's
row) from the receipts bucket into `<mirror dir>` through `wrangler r2
object get` (read only; the operator's own Cloudflare login), each object
before the pointer, each through a temporary name renamed into place. Point
the validator at it with `[fetch] base_url = "file://<mirror dir>/"` and run
the script on a timer (every few minutes is enough: a row changes at most
once a tempo). Every object is verified by the validator as from the
public host. A failed read exits 1 and writes wrangler's exit status and
its whole output (each line prefixed `row-mirror: wrangler:`) to standard
error, so the timer's journal keeps the cause, for example a missing or
expired Cloudflare login.

To upgrade an instance, run the installer with `--from`. Every check that
can refuse runs with the new build before anything is replaced:
`check-config`, and with the sandbox the drop-in render (its bind list and
the stray and breadth refusals below; also on a first install). The binaries,
unit templates and drop-ins it then replaces are copied first and put back
if a later step fails (the sandbox test under the unit's restrictions, for
example), so a refusal never leaves the new build under the old drop-in for
the next automatic restart. A running unit keeps its old process until it
restarts: either stop the unit first and start it afterwards, or pass
`--enable --start`, which restarts a running unit (in a settled window, with
no commit or reveal in flight; the journal recovers an interrupted one
anyway). Without either the installer says that the new build takes effect
at the next restart. A directory of binaries you built yourself
needs `--unverified` even when it holds a `SHA256SUMS` (without the flag the
installer refuses before installing anything); that unsigned digest list is
still checked. The upgrades recorded in `TESTNET-M2.md` predate the
signature check and ran without the flag. Run the same command with
`--dry-run` first: the dry run runs the same read-only checks with the new
build (`check-config` and the drop-in render), so a refusal comes while the
old build still runs. The signature binds the digests,
not a version, so a validly signed older release is still a rollback: before
installing one, stop the unit and run `rollback-check` ([Rolling back past
this rtao pin](#rolling-back-past-this-rtao-pin)).

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
`Restart=on-failure`. Both set `MemoryMax=2G` and `MemorySwapMax=0`. That
limit was derived for the earlier receipt-verifying fetcher and is kept as
an over-provision until the row fetcher is measured: a row session holds at
most the pointer and four signed objects (the row within
`CoordinatorRow::max_signed_len`), and reads at most `SESSION_BYTES` into
memory whatever a host serves, far below the earlier worst case. The
earlier measurement, for the record: the worst case was at the receipt
index bound (a test removed with the receipt fetch path; debug build,
2026-11-03): a target's cohort and its
carrier each with the largest receipt index the protocol can sign (972,591
receipts, a 67,108,947-byte index, so a 35,773-byte v3 bundle verifying
1,945,182 receipts). One fetch session peaked 409 MiB above its starting set,
one verification 543 MiB (with that session's bundle still held), and the
most a continuous run holds at once (the installed verification, the loop's
newest staged bundle, one waiting in the fetcher's channel and a session in
flight) 810 MiB; each verified bundle holds its decoded receipt indexes. With
the 63 MiB start measured at the bundle bound that is about 0.85 GiB, too
close to the earlier 1G. The embedded bound measured less
(2026-11-01: a
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

### Isolation

A validator process should read only its binary, its configuration, the
hotkey files that configuration names and its own state directories. A user
unit otherwise runs with everything its account can read and reach: the
rest of the home directory (other wallets and coldkeys, logins, tokens,
unrelated data), the user manager's environment, and local sockets such as
the session bus (which can start a unit outside these limits), agents and a
container daemon. Three layers address this:

- **The unit templates** start the validator through `env -i`, with only
  `HOME` and `RUST_BACKTRACE`, and set only hardening that needs no
  namespace: `NoNewPrivileges`, `RestrictAddressFamilies=AF_INET AF_INET6
  AF_NETLINK` (no Unix sockets, so no bus, agent or daemon socket),
  `RestrictNamespaces`, `SystemCallFilter=@system-service`,
  `MemoryDenyWriteExecute`, `LockPersonality`, `RestrictRealtime`,
  `RestrictSUIDSGID`, `KeyringMode=private` and `UMask=0077`. systemd's
  mount settings (`ProtectHome=`, `ProtectSystem=`, `PrivateTmp=`,
  `BindPaths=`, `InaccessiblePaths=`, `PrivatePIDs=`) need a user namespace
  in a user unit. Where unprivileged user namespaces are restricted, as by
  Ubuntu's AppArmor default (`kernel.apparmor_restrict_unprivileged_userns
  = 1`), any of them fails the unit with exit 226, so the templates carry
  none. Measured 2026-11-04 on the Ubuntu build host (systemd 259, kernel
  7.0, x86_64). `IPAddressDeny=` has no effect in user units there, so
  loopback and LAN services stay reachable.
- **The bubblewrap drop-in** (`install.sh --sandbox`, default `auto`):
  `slop_ninja_validator sandbox-unit` renders
  `<unit>.d/50-sandbox.conf` for both units from the configuration. It
  starts the same command inside `bwrap` (bubblewrap 0.11.1 tested). The
  sandbox's root holds `/usr`, `/etc`, the `/bin`-style links into `/usr`
  and the resolver's runtime directory, all read-only. It has a private
  `/tmp`, `/dev` and `/proc` (its own PID namespace: no other process, and
  no `/proc/<pid>/root` of one, is reachable) and shares the host network.
  On top it binds the configured paths: the binary, the configuration file
  and the hotkey's `pub.txt` companion read-only; the dispatch keyfile
  read-only (dispatch unit only); the journal's directory read-write; the
  package's directory, read-write with `[fetch]` and read-only without; the
  fetch cache and `rpc.runtime_cache_dir` read-write; and a `file://` mirror
  read-only. A configured path inside a bound directory gets that
  directory's access: a package directory inside the journal's directory
  is read-write even without `[fetch]`. The drop-in's comment lists each
  such path under the bind that covers it. After the binds the sandbox's
  root is remounted read-only, so a write to a path the drop-in does not
  bind fails instead of landing on a temporary root that vanishes at the
  next start. The exceptions are the sandbox's own `/tmp` and `/dev`, which
  are writable and empty at every start: a write there succeeds and is
  lost, and a configured path under `/tmp` gets writable private parent
  directories, so keep configured paths out of `/tmp`.
  `sandbox-unit` refuses a directory bind that is the home directory or
  contains it, one that other tools of the account share (`~/.config`,
  `~/.cache`, `~/.local` and its `state`, `share`, `bin` and `lib`,
  `~/.bittensor`, `~/.ssh`, `~/.gnupg`), one that contains the wallet
  directory, a hotkey file or the configuration, a `..` path and a required
  path that does not exist. It also refuses a directory bound read-write
  that holds anything but this instance's files: in the journal's
  directory its journal files (`<journal>` and `<journal>.*`), its package
  (`<package>` and `<package>.*`, or the directory holding it), its caches
  and a mirror; in a package directory of its own, the package files; in
  the fetch cache its `.lock`, `cohorts` and `authorizations`; in the
  runtime cache rtao's metadata files (`md*.bin`, `.rtao-md-*`). Empty
  directories are ignored. Each such directory is bound whole, so another
  instance's journal, an old package, a key file or another tool's files
  there would be readable and writable inside the sandbox. Give each
  instance a state directory of its own,
  `~/.local/state/slop_ninja_validator/<instance>/`, which the installer
  creates. Inside the sandbox the validator is PID 1, as every sandboxed
  validator is, and rtao's journal names its write temporaries
  `.journal-write-<pid>-<n>`. Journals sharing a directory would collide
  on those names, so a validator that is PID 1 refuses to start (exit 2)
  when another journal beside its own is open in a running process (its
  lock is held). A retired journal's lock, which no process holds, only
  logs `journal_dir_shared`. Before the journal opens, the validator
  removes temporaries a killed process left, holding the journal's lock
  and every other journal's lock in the directory; if another journal
  there is open, it keeps them and logs `journal_temps_unswept`.
  Otherwise a temporary left by an OOM kill, a SIGKILL after
  `TimeoutStopSec` or a power loss would fail every later start at the
  same write with "File exists". As PID 1 a signal with no handler is
  discarded, not deferred, so `run` blocks SIGTERM and SIGINT as its first
  step, registers their handlers and only then unblocks them; a stop that
  arrives while the configuration loads or the journal opens takes effect
  at the run's first wait. Only a signal in the moment between the exec
  and that first step (the dynamic loader) is still lost; the stop then
  ends with `TimeoutStopSec`'s SIGKILL, which the journal recovers from.
  The drop-in sets `SLOP_NINJA_SANDBOX_BINDS` to the unit and a digest of
  its bind list. At startup (`run` and `check-config`) the validator
  derives the bind list from its configuration again and refuses to start
  (exit 2, `sandbox_refused`, not restarted) when the two differ, for
  example after a configured path changed without re-running the
  installer. A PID 1 start without the variable (a drop-in from an earlier
  build) logs `sandbox_unverified`.
  The installer renders each drop-in to a temporary file, then runs
  `sandbox-unit --check` in a transient unit (`systemd-run --user --wait
  --pipe`) with the template's hardening and that drop-in's namespace and
  system-call lists, so the check passes only where the unit can build the
  same sandbox. `--check` runs `check-config` inside the sandbox, which
  includes the startup comparison above, then tests that every bound path is
  readable (and writable where bound so) and that every other entry of
  each directory above a bound path, of the home directory, of the host's
  `/tmp` and of `/run` (with each directory above the resolver's, so
  `/run/user` too) is absent. `/proc` and `/dev` are the sandbox's own and
  are not compared. Its report lists the directories tested and, for each
  directory bound whole, how many entries it shows. Without a drop-in the
  installer instead runs `check-config` under each template's own
  hardening. With `--start` it restarts the dispatch unit if it is running
  (otherwise starts it) and waits, for at most the unit's own start timeout,
  until that start logs `package_verified`: it fails if the unit stops or
  restarts first, and warns (leaving the unit running) when the timeout
  passes first, for example while startup recovery waits on the RPC. A
  failure after `package_verified` is not seen; follow the log. With `auto`, a host without a working `bwrap` gets a warning
  and no drop-in only when none is installed yet. If a drop-in is already
  installed, `auto` fails and keeps it, so isolation is removed only by
  `--sandbox none`. `--sandbox bwrap` always fails without a working
  `bwrap`. Re-run the installer after changing any configured path, and
  restart the unit after installing a bundle by hand only when the package
  directory changed.
- **The environment.** Both units clear it: `env -i` in the templates,
  `--clearenv` in the drop-in. The validator sees only `HOME`,
  `RUST_BACKTRACE` and, in the sandbox, `SLOP_NINJA_SANDBOX_BINDS`. A
  unit's `Environment=` and `EnvironmentFile=`, the user manager's own
  environment and `environment.d` never reach it. This includes proxy
  variables, which this build does not pass. A testnet drill's
  `SLOP_NINJA_DRILL_FAULT` reaches it only when the drill passes it
  explicitly ([Fault-injection hooks](#fault-injection-hooks)).
- **A Unix user of its own** is still the recommendation for every
  validator, and is required wherever the validator shares a host with
  other services or operator credentials. The sandbox shares the operator's
  UID and network. A dedicated account adds the kernel's ordinary
  permission checks underneath. Root can also give it per-UID firewall
  rules for loopback and LAN services, and keep its journal away from the
  operator's. Create it with a home directory, enable lingering for it, copy
  only that validator's hotkey keyfile and `pub.txt` companion into its
  wallet directory (never the coldkey or other hotkeys), and run this
  installer as that user; the units and drop-in work unchanged under its
  user manager.

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

A drill build reads `SLOP_NINJA_DRILL_FAULT` from its environment, and the
units clear the environment ([Isolation](#isolation)), so a unit's
`Environment=` setting no longer arms a drill. A drill passes the variable
explicitly instead. With the sandbox, render a drill-only drop-in with
`sandbox-unit --unit dispatch --binary <binary> --drill-fault
<phase>[:<n>]` into `<unit>.d/60-drill.conf`, which replaces the
sandbox drop-in's `ExecStart=`. Without the sandbox, use a drop-in that
resets `ExecStart=` and adds `SLOP_NINJA_DRILL_FAULT=<phase>[:<n>]` to the
`env -i` arguments. Remove the drop-in after the drill. Each drill must
confirm that its fault fired (exit 86 in the unit's log): a variable that
does not reach the process arms nothing.

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
- the fetcher against an in-memory public host: a published authorization
  and the coordinator's row assemble into exactly the row bundle the
  operator tool writes, reading the pointer and its signed objects only; a
  pointer without a row is `row_unpublished` (retried) and keeps the
  installed bundle; a row of another authorization or with other bytes is
  refused; an earlier layout's cache is left byte for byte; the launch
  pointer, a foreign chain, a hint refresh that is not a newer authorization
  and a re-authorization that is; a pointer revocation journaled before
  anything else, withdrawing the staged authorization, and a forged one
  refused unjournaled; a mirror directory served like the host; and the
  real HTTP transport against a loopback server (range continued or
  ignored, 404, a redirect refused);
- the fetcher's bounds against a hostile host: an authorization of the wrong
  length, or a row past the protocol's bound, is never requested and one
  with a bad signature stops the session before anything else is read; an
  unchanged session reads no object; a trickling HTTP host is cut off by
  the total bound;
- the copy rule: the committed row is the coordinator's signed entries,
  with units of a registration the snapshot no longer holds burned through
  the owner's UID (deferred without one); an upgrade over a retired v1 or
  v3 bundle defers, commits nothing new, and queues the row once a row
  bundle is installed;
- every supersession decision (unchanged, refresh by a newer authorization
  or by coverage, an authorization that is not newer, a re-signed row of
  the same authorization refreshing by its later epoch and an earlier one
  kept out (`row_not_newer`), end to end through a fetch session and the
  loop, a regression to an
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
  installer's dry run and checksum rules, its sandbox modes, and the
  dispatch unit's restart policy and hardening;
- the sandbox drop-in (`sandbox_tests`): exactly the configured paths are
  bound, with their access, refusals for a home-exposing, shared or
  key-holding directory, a `..` path or a missing path, strays in every
  read-write directory, the journal-temporary sweep and the startup guard
  with held and free foreign locks, and unit-file quoting; and end to end
  (`tests/sandbox.rs`, skipped where bubblewrap cannot build a sandbox; its
  scratch homes sit outside `/tmp`): `sandbox-unit --check` against a
  synthetic operator home whose coldkey, other hotkeys, logins, secrets and
  the host's `/tmp` and `/run` entries stay invisible. `deploy/test-install.sh`
  also covers upgrades with a stub bubblewrap: a refused render replaces
  nothing (also in a dry run), and a failed sandbox test puts the old
  binaries, templates and drop-ins back;
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

At rtao `fa83052d` (tag `snapshot-2026-11-05.2`), 109 unit tests and the 4
sandbox tests passed on 2026-11-05 on the development host, with workspace
Clippy and the `testnet-drills` build clean under denied warnings. New: the
dry run's tag basis from rtao's `select_tag` (`native`, `reviewed_code`,
`unreviewed_code`, `code_unknown`, followed in every case), and the rollback
check against `0c128160`, which passes with the queued `Submit` burn row,
holds a pending reveal whose reveal period changed (not retired) and treats
a compacted journal as roll-forward only. The `OwnerRowHeld` retirement
through `retire_pending` is covered at the adapter and in the dispatch
loop's step (wait until finalized, retire, exit 75, the next label; and the
crash between rtao's marks and our reason). No test drives a held queued
row through rtao's real scheduler on a chain, and the new pin has not run
live.

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

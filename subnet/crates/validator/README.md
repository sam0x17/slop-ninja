# Native validator

`slop_ninja_validator` is the native Slop Ninja validator. It builds on the
journaled adapter in [`subnet/`](../../README.md) and rtao `b5553a76`. It
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
9. Read `SubnetOwnerHotkey(netuid)` by name. A row whose positive entries all
   belong to this validator's hotkey or the owner hotkey (the launch burn row,
   or a target whose credited miners all left) is deferred
   (`row_would_be_held`): rtao holds such a row (`HoldPrevious`) and never
   commits it, so this validator commits nothing that tempo and its previous
   weights stay on chain. A row that must burn while the owner hotkey holds no
   UID (`row_deferred`), and an authorization that does not cover the epoch
   (`authorization_refused`, until a renewal or a new bundle), defer the same
   way. A continuous dispatch run waits in the same process for the next
   epoch (with `[fetch]`, fetching every `poll_secs` and continuing at once
   when a renewal or successor is installed) and applies the bundle again;
   a dry run or bounded run exits 75. Inside the dispatch loop the tempo is
   skipped and the next epoch is tried afresh.
10. Without `--dispatch` (the dry run): the journal is not written. Compute
    the operation ID and report whether the journal already retains it
    (`retained`), read the weight preflight at the best block, require that
    rtao's conform step leaves the derived row unchanged (`row_not_preserved`
    otherwise), encrypt it to the predicted reveal round, compose
    `commit_timelocked_mechanism_weights`, type-check it by name against live
    metadata, prepare the unsigned extrinsic for the public hotkey, and print
    its signing-payload hash. No key is read.
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
    signing guard, unlock `[dispatch] hotkey_file`, take one metagraph
    snapshot, connect the signing transport (when configured) last, and for
    each finalized block run `dispatch_finalized` (with bounded recovery when an attempt
    needs reconciliation). Once the commit is finalized it reads back
    `TimelockedWeightCommits(netuid, commit_epoch)` for its `reveal_round`.
    Every completed reveal is compared with the row of the operation that
    committed it, read at the block that observed the reveal
    ([Reveal checks](#reveal-checks)). With `[dispatch] max_finalized_blocks`
    unset the run is continuous: it re-commits the target once per tempo
    until a signal or a check. A fetched authorization for the same target
    with a longer lease replaces the one each re-commit is derived under, in
    place. A fetched successor is installed at its eligible boundary and the
    run continues on it in the same process (`continuing_on_successor`): the
    recovered journal and reveal watcher carry over, so no restart, recovery
    replay or restart delay separates the old target's last commit from the
    successor's first. When the lease ends with `[fetch]` configured, new
    commitments pause (`lease_paused`, `alert = true`) while reconciliation
    continues, until a renewal or successor arrives. Without `[fetch]` the
    run first stays until this target's outstanding reveals have been
    compared (`lease_paused`, `fetcher: false`), then waits as in step 9 and
    reads the bundle file again each epoch.

Before each signature the guard requires that the operation and exact row
were verified in this process, that the call is a timelocked commit (direct
weights are never a fallback), and that commit/reveal is enabled at fresh
best-block state. The adapter's live-surface check runs first.

Any refusal leaves the journal paused, and the journal is paused again before
exit, so the next run recovers first.

| Exit | Meaning | systemd |
| --- | --- | --- |
| 0 | Completed (dry run printed; dispatch revealed under `once`; a bounded run reached `max_finalized_blocks` or installed a successor, `superseded`), or interrupted by a signal | dispatch unit restarts (`Restart=always`); dry-run unit stays stopped |
| 1 | Error, such as an RPC failure | restarts |
| 2 | Invalid configuration, missing journal, or `--dispatch` without `[package]` and `[dispatch]` | stays stopped |
| 3 | Halted by a check: subnet or hotkey not registered, commit/reveal disabled, chain mismatch, recovery that needs review, a refused package or row, or chain weights that differ from the row of the operation that committed them | stays stopped |
| 75 | Recovery still in progress after `recovery.max_rounds`, refused upstream (`upstream_storage_refused`), work awaiting recovery (`pending_operation_unsettled`); and, outside a continuous dispatch run, a deferred `reward-v1` tempo (`row_deferred`, `row_would_be_held`, `authorization_refused`, a dispatch run ending `tempo_deferred:*`) or no bundle installed (`bundle_unavailable`) | restarts |

## Configuration

Start from [`validator.example.toml`](validator.example.toml). Every chain value
is explicit, and no netuid is built in; `netuid = 0` is refused. Paths must be
absolute. Outside `--dispatch`, only the hotkey's `<hotkey>pub.txt` companion
is read; a missing companion is an error.

`[package]` names the bundle path and the three application public keys
(`package`, `receipt`, `authorization`, 32-byte hex, all distinct). They are
not chain keys. `[fetch]` (requires `[package]`) names the public status
host's `base_url` (`https://`, or `http://` to a loopback host; no
credentials, query or fragment), `poll_secs` (300, at least 60, the index
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
its own budget, which is also its burst, because rtao's 30 s window from nonce
lease to pool admission made 26 to 28 calls on testnet. That window alone puts
a commit's minute above about 20 calls on public RPC, an rtao limit. The second
connection makes about 16 calls at once, so it opens after every startup read,
with `signing_connect_quiet_secs` (60 by default) on each side in which the
read session makes no call. Until rtao-rpc serves testnet state,
the example lists the public OpenTensor testnet endpoints (testnet shortcut
T3, removed by upstream item R-1).

Logs are structured JSON on stderr, or text if configured. Events carry fixed
names and counts. They omit addresses, paths, ciphertext, signed bytes and
endpoint paths. The dry-run and dispatch reports on stdout contain the row
entries, which are public (any validator can derive them from the package).
Dependency crates log at `warn` and above.

## Fetcher

`fetch.rs` reads the publisher's layout `v1`
([`docs/testnet/public-layout.md`](../../../docs/testnet/public-layout.md)):
`v1/authorizations/current.json` names the current
authorization and any renewal as content-addressed objects; the cohort index
names each cohort's target and whether its receipts are retained; each
cohort's `publication.json` and `chunk-map.json` list every object's SHA-256
and length. These documents are unsigned and only locate objects: every
object must match its listed digest and length, and the assembled bundle is
then verified by `package_v2::verify` against the held target, like an
installed file.

**Not live yet.** The public host is not deployed, and the coordinator does
not yet write the private pointer and objects the publisher copies to
`v1/authorizations/` (authorizations and renewals are still signed by the
operator tool, T12); until both exist the fetcher reports `unpublished` and
the validator keeps the bundle the operator installs. The pointer, as the
layout defines it:

```json
{
  "schema": "slop_ninja.public.authorization/v1",
  "chain": { "genesis": "<64 hex>", "netuid": 601, "mechanism": 0 },
  "kind": "cohort",
  "authorization": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": 309 },
  "renewal": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": 196 }
}
```

`kind` is `launch` (a `LaunchAuthorization`) or `cohort` (a
`RewardIntentAuthorization`); `renewal` is the newest `RewardIntentRenewal`
of that authorization, or absent. Each object is the coordinator's signed
bytes, named by its SHA-256, and its `bytes` must be the record's exact
signed length (fixed by the schema, `fetch::authorization_lengths`: 230
bytes for a launch authorization, 309 for a reward-intent authorization,
196 for a renewal). The
pointer is rewritten in place (`max-age=60`); the objects are never deleted.

A session reads at most `SESSION_BYTES` into memory: `MAX_BUNDLE_BYTES`
(64 MiB, the file bound of bundle schema v2), documents and objects
together, counting an object read from the cache as well as one fetched.
Before reading any object of a cohort it refuses one whose listed objects
could not fit the bundle (`bundle_too_large`; the bundle stores each object
hex-encoded, so objects get half the bound). Authorization and renewal
objects are read into memory, their length checked against the schema and
their signature verified before anything else is fetched; they are never
cached. Before reading any receipt bundle, the signed receipt index is
opened, the chunk map's receipt root and count must be the index's, the
bundles' total length must equal what the index's entries declare (a 4-byte
length prefix plus each receipt object's signed length), and every bundle
must be listed in `publication.json` under `receipts/` with the chunk map's
digest and length (`chunk_map_mismatch` or `object_unlisted` otherwise).

The bound limits the roster. The bundle embeds every receipt of the target
cohort and, for a carried target, of the carrying cohort, hex-encoded: the
live testnet bundle of 2026-11-01 held 14,344 receipts in about 6.4 MB of
receipt objects and index, about 448 bytes per receipt, so the bound holds
about 150,000 receipts across both cohorts. At about 2,200 receipts per
scored entrant per cohort (1,000 cases, the 1,024-document probe and a
200-case audit draw, before retries; an estimate) a carrier bundle fits
about 34 entrants per cohort: roster 24 leaves about 29% headroom, and
roster 40 does not fit, so every native validator would refuse such a
bundle (`bundle_too_large`) and keep its old target until the lease ends.
Raising it means a new bundle schema that references verified cache
objects by digest instead of embedding them, verified from files.

Each session holds the cache lock (`<cache>/.lock`; another process gets
`cache_busy`, and `fetch` then exits 75), never follows a redirect, sends no
credential, abandons a request idle for `stall_secs`, and ends any request
and the whole session after `poll_secs`. Verified objects stay in the cache
(directories mode 700, files 600); a partial object is kept as
`<name>.part` and continued with a range request (a host that ignores the
range restarts it); a digest mismatch deletes it. Every session that
located a cohort then prunes the cache to the cohorts it named and those
the installed bundle was built from, whatever its outcome, so a host that
lists junk cannot grow it past one session's reads. A session whose
documents and held target are byte-identical to those of the last session
that produced a bundle reports `unchanged` and reads no object; signature
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
installed one; the same target with a longer lease, or one covering the
finalized epoch where the installed one does not, is a refresh (installed,
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

Fetched bundles are written in bundle schema v2, whose `shortcut` field
must read `T12`: the label names the bundle format the operator tool also
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
without `SHA256SUMS`, for binaries you built yourself). No release signing
key is published yet (decision D14): until it is, verify the release's
detached signature over `SHA256SUMS` by the release instructions before
running the installer. It installs the configuration with mode 0600 only
when none exists, runs
`check-config`, creates the journal with `--init` (`init` refuses an existing
path, so a typo never replaces history), enables lingering
(`loginctl enable-linger`), reloads the user manager and enables
`slop_ninja_validator-dispatch@<name>`. Back up the journal with the wallet.
It holds sensitive commit material, so do not publish it.

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
`Restart=on-failure`. Both set `MemoryMax=1G` and `MemorySwapMax=0`, from a
measurement at the bundle bound (`fetch_tests::peak_memory_at_the_bundle_bound`,
ignored by default; debug build, 2026-11-01): with a 66,437,318-byte bundle
(148,235 receipts, just under the 64 MiB bound) one fetch session peaked
217 MiB above its starting set, and the most a continuous run holds at once
(the installed verification, two staged bundles and a session in flight)
386 MiB above a 63 MiB start. A live dispatch run with a 15,344-receipt bundle
peaked at 56 MiB. A larger bundle bound needs a new measurement. Keep
`recommit = "per_tempo"` under the dispatch unit: with `once`, each restart
commits again. Stop the unit before using the offline journal CLI, because
the journal holds a lifetime lock:

```sh
systemctl --user stop slop_ninja_validator-dispatch@<name>
slop-ninja-journal --journal <journal_path> inspect
```

Never delete the journal or its lock to clear a refusal.

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
SLOP_NINJA_TEST_TMPDIR=<task tmp> cargo test --manifest-path subnet/Cargo.toml --locked -p slop_ninja_validator -- --test-threads=1
cargo clippy --manifest-path subnet/Cargo.toml --locked --workspace --all-targets -- -D warnings
```

The tests are offline. They use fake chain ports, real file journals and
public deterministic dev seeds, and create no client, signer or RPC
connection. They cover:

- package verification: every tampered envelope, receipt object, missing or
  reordered receipt, cross-cohort record, wrong role, untrusted key, wrong
  subnet or chain, and malformed bundle is refused before a row exists;
- row derivation, including UID reuse (credit to UID 0) and a foreign UID 0
  (no row);
- the signing guard's verified-row, contract and timelocked-call checks;
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
  outside the chain are refused; the launch pointer, a foreign chain, a
  renewal's longer lease; and the real HTTP transport against a loopback
  server (range continued or ignored, 404, a redirect refused);
- the fetcher's bounds against a hostile host: an authorization of the wrong
  length is never requested and one with a bad signature stops the session
  before anything else is read or cached; a receipt bundle the signed index
  does not account for, another receipt root, or a bundle the publication
  record does not list is refused before any bundle is read; every session,
  refused ones included, prunes the cache to what it named and the installed
  cohorts; an unchanged session reads no object; a trickling HTTP host is
  cut off by the total bound;
- every supersession decision (unchanged, refresh by lease or coverage, a
  shorter lease, a regression to an earlier target, a successor before
  activation, waiting on in-flight work, a switch retiring never-signed
  work), the install order (intermediate, bundle, successor's record), and
  a stale pointer after a switch refused by the held record and by the
  decision;
- the dispatch loop's fetch decisions through a fake loop: wait without a
  chain read while work is in flight, then switch, then compare the old
  target's last reveal with its own row from the reveal ledger (never the
  successor's), observe an unattributed reveal without halting, and refresh
  a lapsed lease in place; the ledger itself (operations that never commit,
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
row, the per-tempo authorization inside and past its lease, and the rtao
hold. Cohort bundles are covered by the protocol crate's `target_bundle`
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
no-fetcher lease-end wait, 73 tests passed on 2026-11-02 (the memory
measurement above is the one ignored test). The fetcher has not run against
a live public host, which is not yet deployed (the authorization pointer is
not published), and no continuous run has crossed a cohort boundary on
subnet 601 yet; the persistent unit's live runs are in
[TESTNET-M2.md](TESTNET-M2.md#10-persistent-unit-2026-11-01).

# Testnet validator guide (experimental)

This guide is for running the native validator `slop_ninja_validator` on
Bittensor testnet subnet 601. Testnet is announced for miners first; outside
validators are welcome to follow along, but their participation is not yet
supported end to end, and this guide is marked experimental. Testnet has no
monetary value; see the [terms](terms.md).

## What a validator does

Slop Ninja operates one lead validator, the coordinator. It runs the hidden
evaluation of every cohort, derives the native weight row, and signs and
publishes that row off chain. Every other validator:

1. reads the unsigned authorization pointer from the public status host,
   which only locates objects, and verifies the signed authorization and
   coordinator row it names against pinned public keys;
2. journals the signed revocation set the pointer names;
3. commits that row on chain through its own journaled, timelocked
   commit-reveal, at every tempo while the row's target is active, and keeps
   committing the last signed row through coordinator silence. Its only
   change to the row burns the units of a registration that no longer
   exists ([What the validator verifies](#what-the-validator-verifies)).

Validators work in parallel. None waits for the coordinator's own chain row,
and the coordinator's chain row and certificate are later consistency
audits, not prerequisites. The only thing other validators contribute is
timely on-chain submission and reveal of the coordinator's row. A validator
may check models in its own way and publish its own scores; those are
advisory and never change its row.

### What a validator never receives

A validator never receives the held-out documents, their labels, per-document
predictions, the private probe, or any private model weights. The validator
releases built before the copy model also receive values derived from the
hidden evaluation: the signed score package carries exact aggregate scores and
near-copy statistics, and its receipts carry each attempt's outcome class,
which an entrant's artifact can steer, so they can reveal information about
the hidden documents and their labels.
Validators built from this tree receive none of those: they commit the
coordinator's signed row under its signed authorization. The row record
carries the chain binding, the reward rule, its source (the launch, or the
cohort's number, label and manifest digest), the target, authorization and
manifest digests, the activation, issue and commit epochs, the finalized
block it was derived at, the owner hotkey, and per paid UID its hotkey,
registration block and integer weight: public chain data and digests, no
score. The entries are committed as signed, except that the units of a
registration the validator's own finalized snapshot no longer holds burn
through the owner's UID. No zero-knowledge proof or independent re-run of
the hidden evaluation is part of the protocol.

## What is live

| Piece | State |
| --- | --- |
| `slop_ninja_validator` startup checks, `observe`, the dry run and journaled dispatch | built and tested on Linux x86_64; live commits, per-tempo re-commits and fault drills ran on subnet 601 on October 30, 2026, under a fixture package |
| Verification of the coordinator's signed row (`slop_ninja.coordinator_row_bundle/v1`; earlier package bundles are refused as `bundle_schema_retired`) | in the current source (build from source, [section 2](#2-install)); validator release binaries follow in a later release |
| The coordinator's signed row | built and tested offline; the coordinator build that signs it is not yet deployed |
| Published rows on the public status host | not yet live |
| The row fetcher (`[fetch]`) | built and tested offline against the publisher's layout; waits for the public host, so until it is live every validator installs each row bundle by hand or reads a local mirror (below). The coordinator signs each closed cohort's authorization itself, so the pointer names the current one with no operator step |
| Continuous dispatch, successor supersession and persistent user units | built and tested offline; not yet run unattended across cohorts on subnet 601 |
| Committing the launch burn row | live on subnet 601: both Slop Ninja validators commit and reveal it each tempo under rtao's owner-row opt-in (below); first native commit and reveal on November 3, 2026 |
| Release public key | published on the [landing page](README.md#release-key) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Validator release binaries (Linux x86_64) | not in the first release, which holds only the miner client and the artifact checker; a validator release follows later. Until then, build from source ([section 2](#2-install)) |

Today an outside validator can install the binary, create its journal, run the
startup checks and use `observe`. Committing rows waits for the pieces above.

## 1. Requirements

- Linux x86_64, the tested platform. macOS is untested.
- Rust 1.89 or later to build from source; the tested toolchain is 1.99.1.
- A testnet coldkey and a registered validator hotkey on subnet 601, with
  enough stake for a validator permit. Permits go to the highest-stake hotkeys,
  up to the subnet's validator limit; read the current values with rtao before
  staking.
- A persistent directory for the journal, backed up with the wallet.
- WebSocket RPC access. Until a dedicated testnet RPC service is available, the
  example configuration lists the public OpenTensor testnet endpoints.

## 2. Install

The first release holds no validator binaries; a validator release follows
later. Until then, build from a checkout of the
[public repository](https://github.com/sam0x17/slop-ninja):

```sh
cd <checkout>/subnet
cargo build --release --locked -p slop_ninja_validator -p slop-ninja-subnet
install -m 0755 target/release/slop_ninja_validator target/release/slop-ninja-journal ~/.local/bin/
```

`slop_ninja_validator` is the validator. `slop-ninja-journal` inspects and
pauses the journal offline. The build uses rtao from Git at the pinned revision
and needs no Python or btcli.

Register and stake with rtao, as the [miner guide](miner-guide.md#4-register-on-subnet-601)
shows for a miner hotkey:

```sh
rtao subnets register --network test --netuid 601 --wallet my-wallet --wallet-hotkey my-validator
rtao stake add --network test --netuid 601 --wallet my-wallet --hotkey my-validator --amount-tao <amount>
rtao subnets metagraph 601 --network test
```

Subnet 601's pool is small, so a large stake moves the price; the CLI's default
slippage protection refuses a fill that moves it more than 5%
(`SlippageTooHigh`). Check each flag with `rtao stake add --help`.

## 3. Configure

Start from [`validator.example.toml`](../../subnet/crates/validator/validator.example.toml),
copy it to `~/.config/slop_ninja_validator/<instance>.toml` with mode 0600, and
set every value. Paths must be absolute. The file holds no secrets. The
example ships with `netuid = 0`, which is refused, so an unedited copy never
runs; set `netuid = 601`. Its `[package]`, `[fetch]` and `[dispatch]` sections
are commented out: uncomment them when a row is available (from the public
host, a local mirror or a bundle you install by hand) and you want to sign.
Edited for subnet 601, the values that matter look like this (the example's
`[recovery]` and `[log]` sections keep their defaults):

```toml
network = "test"
netuid = 601
weight_version = 0          # WeightsVersionKey(601) read 0 when unset
journal_path = "/home/operator/.local/state/slop_ninja_validator/weights-journal.json"

[wallet]
path = "/home/operator/.bittensor/wallets"
name = "my-wallet"
hotkey = "my-validator"     # only <hotkey>pub.txt is read outside --dispatch

[rpc]
endpoints = [
  "wss://test.finney.opentensor.ai:443",
  "wss://test.chain.opentensor.ai:443",
]
max_requests_per_minute = 15

[package]
path = "/home/operator/.local/state/slop_ninja_validator/package.json"
[package.trusted_keys]
package = "<package_public_key from testnet-trust.json>"
receipt = "<receipt_public_key from testnet-trust.json>"
authorization = "<authorization_public_key from testnet-trust.json>"

[fetch]
base_url = "https://<public status host>/"

[dispatch]
hotkey_file = "/home/operator/.bittensor/wallets/my-wallet/hotkeys/my-validator"
```

- **Trusted keys.** Take the three keys from the release's
  `testnet-trust.json` (schema `slop_ninja_testnet_trust_roots_v1`). They are
  Ed25519 application keys, never chain keys. Pin them; never trust a
  `keys.json` published beside a bundle. The keys in the repository's
  `subnet/coordinator/testnet-trust.json` today are rehearsal keys; they will
  be rotated before the testnet is announced, so use the file from the signed
  release.
- **`[package]`** names the bundle file the validator verifies. With
  `[fetch]`, the validator writes it there itself; without `[fetch]`, you
  install it. Without `[package]`, `run` stops after startup.
- **`[fetch]`** names the public status host (`https://`, with no
  credentials, query or fragment), or a local mirror `file:///<dir>/`
  ([below](#fetching-the-coordinators-row)). The validator checks it at
  every start and then every `poll_secs` (300 by default, at least 60, the pointer's
  cache lifetime) during a dispatch run; see [Fetching the coordinator's
  row](#fetching-the-coordinators-row). `stall_secs` (60) abandons a
  request that receives nothing for that long; it resumes at the next check.
  `cache_dir` (default `public-cache` beside the journal) holds only the
  fetcher's lock and what an earlier release cached there; a row fetch
  stores nothing on disk.
- **`[dispatch]`** names the keyfile that `run --dispatch` signs with. Its
  `pub.txt` must name the `[wallet]` hotkey. Without `--dispatch` the keyfile
  is never opened. Leave `max_finalized_blocks` unset: the run is then
  continuous. Set it only to bound a drill run.
- **`epoch_offset_blocks`** chooses the block in each epoch at which the
  commit falls due. Unset (the default), rtao's per-hotkey jitter chooses it,
  which spreads validators' commits; keep it unset on an RPC service without a
  tight per-address budget. On a public endpoint with a budget of about 15
  requests a minute, set it late in the epoch: rtao's reveal watcher replays
  every finalized block between a commit and its reveal, at about 14
  requests a block, so about one block a minute on that budget, and makes no
  new attempt until it has caught up. At offset 320 (testnet's value; tempo
  360, so 361 blocks an epoch) that is 25 to 45 blocks, about 23 to 42
  minutes, and one commit per tempo fits; at offset 20 it is about 340
  blocks, several hours, longer than a tempo. Leave room
  after the offset for the commit to be signed and finalized before the
  epoch ends. It must be at most the subnet's `Tempo` (an epoch spans
  `Tempo + 1` blocks), or no commit ever falls due; the validator then logs
  `epoch_offset_beyond_tempo` with `alert = true`.
- **RPC.** Endpoints must be `wss://` with no credentials, path or query. The
  whole process shares one request budget (`max_requests_per_minute`, 15 by
  default). Public pools return HTTP 429 at about 25 requests per minute from
  one address, and a commit's minute can exceed about 20 calls; an optional
  `signing_requests_per_minute` gives the signing executor its own connection
  and budget.

## 4. Check, initialize and observe

```sh
C=~/.config/slop_ninja_validator/my-instance.toml
slop_ninja_validator --config $C check-config
slop_ninja_validator --config $C init
slop_ninja_validator --config $C observe
```

- `check-config` validates the file offline and prints a redacted summary.
- `init` creates the journal once and refuses an existing path, so a typo never
  replaces history.
- `observe` is read-only: epoch state, this hotkey's timelocked commits for
  recent epochs (`--epochs`, default 3), its `Weights` row, and `Incentive` at
  the row's UIDs. It opens no journal or key.

## 5. Run

```sh
slop_ninja_validator --config $C run              # dry run: nothing signed
slop_ninja_validator --config $C run --dispatch   # signs and submits
```

Every run starts the same way, against one finalized block:

1. Opens the journal (opening persists a pause) and requires its genesis.
2. Evaluates the signed weight operations and the storage items rtao reads
   against the live metadata, by name. No runtime spec or transaction version is
   compared anywhere; an operation with no type-correct encoding is deferred and
   re-evaluated, never a halt.
3. Recovers the journal through rtao without a signer until it is ready to
   resume.
4. Reads at the finalized block that the subnet exists, that the hotkey has a
   UID, and that commit-reveal is enabled (below).
5. Resumes explicitly. With `[fetch]`, runs one fetch and installs what it
   verified (below). Then verifies the installed bundle, takes its signed
   row and either prints the dry run or dispatches.

Without `--dispatch`, the journal is not written: the run takes the row,
encrypts it to the predicted reveal round, composes the timelocked commit,
type-checks it against live metadata and prints its signing-payload hash. No
key is read. With `--dispatch`, the row is enqueued as a journaled operation,
the keyfile is unlocked, and each finalized block drives the scheduler. Once
the commit is final the validator reads it back; once the reveal is observed
it compares the on-chain `Weights` row with the row that commitment carried.
The run then re-commits the installed row once per tempo, checked against
that tempo's finalized registrations ([What the validator
verifies](#what-the-validator-verifies)), until it is stopped. A
successor target, once installed, takes over in the same process; a tempo
with nothing to commit (below) is waited out in the same process too.

| Exit | Meaning | Dispatch unit |
| --- | --- | --- |
| 0 | completed: the dry run printed; a bounded run reached its block bound or installed a successor (`superseded`); or a signal | restarts |
| 1 | an error, such as an RPC failure | restarts |
| 2 | invalid configuration, a missing journal, or `--dispatch` without `[package]` and `[dispatch]` | stays stopped |
| 3 | halted by a check: subnet or hotkey not registered, commit-reveal disabled, chain mismatch, recovery that needs review, a refused bundle or row, or chain weights that differ from the row of the commitment that set them | stays stopped |
| 75 | wait and retry: recovery still in progress, refused upstream (`upstream_storage_refused`), or work awaiting recovery; outside a continuous dispatch run, also a deferred tempo, no bundle installed yet (`bundle_unavailable`) or an installed bundle of a retired schema (`bundle_schema_retired`); a continuous run exits on a retired bundle only when a successor waits on in-flight work (see upgrading, under [Fetching the coordinator's row](#fetching-the-coordinators-row)) | restarts |

### Persistent operation

User systemd units are in
[`subnet/crates/validator/deploy/`](../../subnet/crates/validator/deploy):
`slop_ninja_validator-dispatch@<instance>` runs `run --dispatch`
continuously, and `slop_ninja_validator@<instance>` runs the dry run. The
dispatch unit has `Restart=always`, so every exit except 2 and 3 restarts it
after a fixed 60 seconds; exits 2 and 3 need you: fix the cause, run
`systemctl --user reset-failed slop_ninja_validator-dispatch@<instance>`,
and start it again. (The delay is fixed because systemd's stepped backoff
never resets while a unit keeps restarting, so it would leave every later
restart at its maximum.) A continuous run does not exit for routine work:
it continues on a new target, and waits out a tempo with nothing to commit,
in the same process. Both set
`MemoryMax=2G` and `MemorySwapMax=0`. The limit was derived for an earlier
fetcher that verified receipts and is kept as an over-provision: a row
fetch session reads at most 64 MiB into memory. The dispatch unit is enabled for
`default.target`, and lingering starts your user manager at boot, so the
validator survives logout and reboots. The installer does all of this:

```sh
cd <checkout>/subnet/crates/validator
deploy/install.sh --instance my-instance --from <directory with the release binaries> \
  --config <your edited configuration> --init --enable --start
```

Run it from your repository checkout, never from a copy that came with a
release: the release key it trusts is the file beside it, so a script and key
shipped inside a release could vouch for that release themselves. The
checkout is also how a replaced or withdrawn release key reaches you: the
installer has no other revocation path, so update your checkout before
installing a new release. Until a validator release is published, install
the binaries you built in section 2 with
`--from <checkout>/subnet/target/release --unverified`.

It installs the two binaries, the unit templates, the configuration with
mode 0600 (it never replaces
an existing one), and the state directory; runs `check-config`; creates the
journal with `--init`; enables lingering (`loginctl enable-linger`); and
enables and starts the dispatch unit. `--dry-run` prints each step instead.
It runs on Linux only. Before it installs anything, it checks the release:
the directory must hold `SHA256SUMS` and the release's detached signature
over it, `SHA256SUMS.sig`. The installer verifies the signature with
`ssh-keygen -Y verify` (OpenSSH 8.1 or later, `ssh -V`) against the release
key pinned beside it in the checkout, `deploy/release-allowed_signers`, which
is the allowed-signers line on the [landing page](README.md#release-key); it
never reads the copies of the key a release carries. It then requires
`SHA256SUMS` to list each of the two binaries exactly once and both to match
it, and installs the copies it checked. It prints the `Good "file"
signature` line on success and refuses, installing nothing, on any failure.
`--unverified` is for binaries you built yourself: it allows a directory
without `SHA256SUMS.sig` (or without `SHA256SUMS` too), but a signature or
digest list that is present must still pass. Before it replaces anything it
also runs each checked binary once (`--help`), so a release this host
cannot execute leaves the installed binaries in place. The release binaries are
Linux x86_64 (glibc 2.34 or later), the only tested platform; elsewhere, build from
source and pass `--unverified`. Once the unit runs:

```sh
systemctl --user status slop_ninja_validator-dispatch@my-instance
journalctl --user -u slop_ninja_validator-dispatch@my-instance -f
```

Stopping the unit pauses the journal; retained work survives, and the next
start recovers first. Keep `recommit` at its default, `per_tempo`: with
`once`, every restart commits again.

## Fetching the coordinator's row

The fetcher reads one document from the public status host, in the
publisher's layout `v1` (see [the public layout](public-layout.md)), and the
signed objects it names. `v1/authorizations/current.json` (schema
`slop_ninja.public.authorization/v2`) lists, each as a content-addressed
object with its SHA-256 and its length:

- the authorization validators commit under now (`launch` or `cohort`) and
  any refresh of its renew-by hint;
- the revocation set: the latest signed revocation of every revoked target,
  concatenated;
- `row`: the coordinator's signed row (`CoordinatorRow`, under the
  authorization key) under that authorization; [What a validator never
  receives](#what-a-validator-never-receives) lists its fields.

Nothing else is read, not even the public cohort listing or the cohort
summaries. Cohort records, score packages, receipts, receipt indexes and
chunk maps stay with the coordinator. A
validator release from before the copy model does not read `row` and cannot
install a cohort target from this layout: upgrade it.

The coordinator signs each closed cohort's authorization itself. It derives
the installed target's row from its own finalized snapshot once a tempo and
whenever a target is installed, independent of its own commitment, and
signs a new row record only when the paid entries change. Until a row
exists under the current authorization, the session reports `row_unpublished` and retries at the next check, and the
installed bundle keeps committing. **The public host is not deployed yet**;
until it is, the fetcher reports `unpublished` and the validator keeps the
bundle you install. `base_url` may instead name a local directory holding a
copy of the layout (`file:///<dir>/`), checked exactly as the host is;
whatever fills it writes each object before the pointer, the pointer last.
An operator with read access to the coordinator's private bucket can fill
one with
[`deploy/row-mirror.sh`](../../subnet/crates/validator/deploy/row-mirror.sh)
on a timer (every few minutes is enough: rows change rarely, and a
validator commits once a tempo).

The pointer is unsigned and only locates objects. Each object must match its
listed SHA-256 and length before it is used; the signed records are then
verified under your pinned keys exactly as for a bundle file you installed
([What the validator verifies](#what-the-validator-verifies)). A lying host
can make a fetch fail, never change what is verified. It also cannot fill
your memory: an authorization's, renewal's or revocation's listed length must
be its schema's exact signed length (for the set, a whole multiple of one
revocation's), a row's at most the protocol's longest row, and a longer
listing is refused before it is requested; a session reads at most 64 MiB
into memory and ends after `poll_secs`. The revocation set is journaled
before anything else (below). A session whose pointer and held target are
unchanged since the last session that produced a bundle reports `unchanged`
without reading any object other than the revocation set, which every
session reads and journals first. Nothing is cached on disk; only a bundle
the validator installs is written, to `[package] path`. One session at a
time holds the cache lock: `fetch` exits 75 while the running unit holds it.
A refusal of records the host served, or an object that fails its listed
digest, sets `alert` in the session's outcome; an outcome that is only
incomplete does not. Where it is logged depends on which session saw it.
The background sessions of the dispatch loop log an alert as
`fetch_refused` with `alert = true` (warning level) and anything else as
`fetch_kept`, once per change. The session a run makes at startup, and the
sessions it makes while it waits after a deferred tempo (for example over
a retired bundle, below), log their whole report at info level as
`fetch_startup` or `fetch_waiting`, with `"alert": true` inside the report
and no separate `fetch_refused` event; `fetch` prints the same report. An
alert filter that matches only `fetch_refused` misses those.

The fetcher assembles the three objects into a coordinator-row bundle
(schema `slop_ninja.coordinator_row_bundle/v1`) and verifies it against the
target your validator holds. What the validator does with a verified
bundle:

- **The same bundle again**: nothing changes.
- **A re-signed row of the same target and authorization** (the coordinator
  signs a new row when the paid entries change: a deregistration, a
  re-registration, a UID changing hands, an owner move): installed when it
  is later by its commit epoch and derivation block; one that is not later
  is kept out (`row_not_newer`), so a stale or replayed pointer never rolls
  the row back.
- **The same target under a newer authorization** (issued later: a newer
  carrying cohort's authorization, or a re-authorization after a
  revocation): it is installed at once, and a continuous run uses it from
  the next tempo. Operation IDs bind the target, not the
  authorization, so nothing already queued changes.
- **A successor target**: it is installed at the eligible boundary, once the
  finalized epoch reaches the successor's activation (the closing boundary of
  its cohort). While any commitment of the old target is in flight (signed
  and not yet settled by finalized recovery), the switch waits, and no new
  commitment of the old target is queued meanwhile; work that never signed
  is retired as `superseded`. The run continues on the new target in the
  same process. The old target's last commitment still reveals afterwards;
  its `Weights` are compared with that commitment's own row, which the
  validator records beside the journal (`<journal>.reveal-rows.json`). A
  validator that fell behind by several cohorts moves straight to the
  current row's target the same way.
- **A target that activates no later than the installed one** (a stale or
  rolled-back pointer) is never installed. Fetched, its row fails
  verification against your held target, an alert (logged as above:
  `fetch_refused` from the dispatch loop, or `"alert": true` inside a
  `fetch_startup` or `fetch_waiting` report); the run continues and the
  installed row keeps committing.

**Upgrading from a release that verified score packages.** Bundles of a
cohort's records and receipts (`slop_ninja.reward_v1_bundle/v1` and `/v3`)
and the fixture bundle `slop_ninja.fixture_package/v1` are retired. One
still installed when the new build starts is a deferral
(`bundle_schema_retired`), never a halt and never a commit: recovery and the
fetcher keep running, and nothing new commits until a row bundle is
installed (a continuous run waits for the fetcher; any other run exits 75
for a later retry). A row bundle of the target your validator already holds
is installed in place over the retired one at once, even with a commitment
in flight (that commitment belongs to the same target), so the next tempo
commits without a restart. A successor target that must wait for in-flight
work over a retired bundle exits 75 instead, and the restart's finalized
recovery settles that work. The new build never touches the earlier
release's fetch cache (`cohorts/` and `authorizations/` under `cache_dir`):
a rollback to that release needs the receipts its installed bundle
references, which may exist only there. Keep it until the new release is
accepted, then remove it with the unit stopped
(`rm -r <cache>/cohorts <cache>/authorizations`). Install the new release
only once the coordinator publishes rows.

**No authorization expires.** Your validator keeps committing the installed
target for as long as it is the held target, however long the coordinator is
silent. The record's `renew_by_epoch` is only a hint. An authorization stops
only when a newer one supersedes it or a signed revocation withdraws it:

- the fetcher journals a revocation that the pointer names before anything
  else (`revocation_recorded`, `alert = true`), and you can install one
  yourself with `slop_ninja_validator --config <config> revoke <file>`; both
  are checked under your pinned authorization key and kept in
  `<journal>.revocations.json`;
- once a revocation withdraws the installed authorization, new commitments
  pause (`authorization_revoked` / `authorization_paused`, logged with
  `alert = true`), work of that target that never signed is retired and no
  new attempt is signed for it, while reconciliation and reveal checks
  continue; a newer authorization or successor resumes commitments. Without
  `[fetch]`, the run first stays until the reveals of its last commitments
  have been compared, then waits for the next epoch in the same process and
  reads the bundle file again.

**Trust-root rotation and restarted chain records.** Your held-target record
names the trust roots it was written under. After a rotation (new keys in
`[package.trusted_keys]` from a new `testnet-trust.json`), the old record is
set aside automatically and the validator starts again from the launch burn
row. When the chain record restarts under the same keys (announced on the
status page), reset it yourself:

```sh
systemctl --user stop slop_ninja_validator-dispatch@my-instance
slop_ninja_validator --config $C reset-target
systemctl --user start slop_ninja_validator-dispatch@my-instance
```

`reset-target` refuses while the unit holds the journal. It renames the
held-target record and the installed bundle (and a staged `<bundle>.next`
left by an interrupted install) aside (suffix `.reset-<seconds>`), and
leaves the journal alone.

`slop_ninja_validator --config $C fetch` runs one session without the
journal or the chain and prints a content-free report: 0 with a verified
bundle (not installed; `run` decides), 75 when the records are not complete
on the host yet or the running unit holds the cache lock, 3 when the
protocol refused them.

## What the validator verifies

Before any row exists, the validator verifies the installed bundle and
refuses it (exit 3) on any failure (a bundle of a retired schema is a
deferral, not a refusal; see upgrading, under [Fetching the coordinator's
row](#fetching-the-coordinators-row)):

- the authorization (launch or reward-intent), any renew-by hint refresh
  and the coordinator's row each authenticate under the pinned
  authorization key before they are decoded, and name this validator's
  chain genesis and netuid;
- the row names the authorization's record digest, target, carrying cohort
  and manifest digest, and issue epoch;
- a launch row is accepted only while the held target is the launch burn
  row; a cohort row's target is the held one with the same activation
  epoch, or one that activates later.

Every per-tempo commitment checks the authorization again for its epoch and
against your journaled revocations; it has no end epoch.

It then reads the subnet's registrations (`SubnetworkN`, `Keys`,
`BlockAtRegistration`, `SubnetOwnerHotkey`) at the same finalized block. The
row it commits is the signed entries, with one rule that uses no score, the
rule the coordinator's own derivation applies: the burn entry, and every
entry whose UID, hotkey and registration block the snapshot no longer holds
(a deregistration, a re-registration, the UID handed to another hotkey), go
to the owner hotkey's UID, which burns them. Each moved entry is logged
(`row_uid_moved`); when units must burn and the owner hotkey holds no UID,
the tempo is deferred (`row_deferred`). A UID that changed hands is never
paid to its new holder, through coordinator silence included. Nothing else
changes a UID or a weight.

The validator does not see the hidden evaluation: no documents, labels,
executions, scores or receipts. It recomputes nothing; what it commits is
the coordinator's signed row.

## Commit-reveal

Subnet 601 runs native weight commit-reveal. The validator:

- reads `CommitRevealWeightsEnabled(601)` at startup from finalized state, and
  halts with `commit_reveal_disabled` if it is off;
- checks it again from fresh state before every signature;
- signs only timelocked commits (`commit_timelocked_mechanism_weights`); a direct
  set-weights call is never a fallback.

If the flag is ever disabled, new commitments pause; nothing falls back to
plaintext weights. Reveals are timelocked to a future drand round and happen on
chain without the validator, so they can complete while the coordinator is
silent. Commit-reveal hides each submission until its reveal, but the
coordinator's signed row is public once published, so anyone can commit the
same row: it neither prevents copying nor proves anything about a
validator's own checks.

The installed row is committed once per tempo, about 20 commitments a day,
each checked against a fresh finalized registration snapshot. During a
coordinator outage, validators keep re-committing the last active target for as
long as the outage lasts: no authorization expires. Only a newer authorization
or a signed revocation changes what they commit. Cohorts are delayed, never
skipped.

### The launch burn row

Until some entrant beats the reference by the margin, the active target is the
burn row: the whole pool goes to the subnet owner hotkey's UID. rtao holds a
row whose only positive entries are the owner hotkey's UID or the validator's
own unless the work opts in to submitting owner rows. The validator opts in
only for the burn row under a valid launch authorization, or a verified target
whose row pays only the owner UID, and records that choice in the journal with the work, so
a restart commits and reveals the same row. Every other row keeps the default.
A row rtao would still hold, such as one that pays only this validator's own
UID, defers the tempo (`row_would_be_held`, exit 75 outside a continuous run),
and the previous weights stay on chain.

Before it queues the burn row, the validator reads the subnet's
`MinAllowedWeights`, `MaxWeightsLimit` and validator permits. A validator
that is not the owner can commit a one-entry row only while the effective
`MinAllowedWeights` is 1; otherwise the tempo is deferred
(`burn_row_min_allowed_weights`). Under a `MaxWeightsLimit` below 65535 the
row may list only the owner UID (`burn_row_weights_limit` otherwise; burn
rows list no other UID). A validator that is not the owner needs a validator
permit (`burn_row_no_permit` otherwise). The chain checks these again when
it reveals the commit, so they must not change between commit and reveal.
Each of these deferrals is logged with `alert = true`.

A build at an rtao pin before `0c128160` cannot open a journal holding queued
burn-row work, and in steady state the next tempo's burn row is almost always
queued, so the work never simply finishes. A build at `0c128160`, the pin
before the current one, opens it; it refuses only a pending reveal whose
reveal period changed while it was pending, until that reveal settles. To
roll back, stop the unit, run
`slop_ninja_validator --config <config> rollback-check --retire`, and install
the older build only when it exits 0. Once the journal retains a finalized
failed weight call, even a reviewed or settled one, it never exits 0: every
release before `review-failure` existed halts on such a journal and cannot
clear it, so roll forward only (see the validator README,
[Rolling back past this rtao pin](../../subnet/crates/validator/README.md#rolling-back-past-this-rtao-pin)).
A valid release signature does not make a build newer: the signature binds
the binaries' digests, not a version, so the same steps apply to an older
signed release.
**Live on subnet 601.** On November 3, 2026 a Slop Ninja validator that is
not the owner, running at rtao `0c128160`, committed the launch burn row
timelocked in block 8,362,443 and revealed it at 8,362,485 with the reveal
matching its journaled row. Both Slop Ninja validators, that one and the
coordinator, now commit and reveal it each tempo; read back at block
8,363,204, the chain held both rows as weight 1.0 on the owner UID and
nothing for any miner UID. They commit it under the launch authorization,
which, like every authorization, does not expire
([Fetching the coordinator's row](#fetching-the-coordinators-row)): it stops only
when a successor target supersedes it or a signed revocation withdraws it.

## The journal

The journal records every signing attempt, nonce, submission and reveal, so a
restart or upgrade never duplicates or loses a commitment. It holds sensitive
commit material: back it up with the wallet and never publish it. It holds a
lifetime lock, so stop the unit before using the offline tool:

```sh
systemctl --user stop slop_ninja_validator-dispatch@my-instance
slop-ninja-journal --journal <journal_path> inspect
```

Never delete the journal or its lock to clear a refusal. A refusal leaves the
journal paused, and the next run recovers first.

### A rejected commit (`recovery_requires_review`)

If the chain includes a weight commit but refuses it, nothing is committed and
the validator records a finalized failure. Recovery reads the failure's
receipt to learn the error. The validator does not sign while a receipt is
unread. If the read fails for a reason that can pass (a timeout or a lost
connection), the validator logs `failure_receipt_unreadable` as an alert once,
keeps running and reads the receipt again at the next block. Startup instead
exits 75 (`recovery_incomplete`), and the unit restarts. If the read cannot
succeed on that endpoint (`state_discarded`: the node pruned the block's
state; `block_not_found`; or `decode` and `metadata`, when the block does not
decode), the validator records `receipt_unavailable` and halts for review as
described below. Each later start reads the receipt again, so pointing the
validator at an endpoint that holds the state (an archive node) and starting
it again clears the halt without a review.

Some refusals settle by themselves: a rate limit, a commit too soon after the
last one, or stake below the weights minimum. The validator records the
decoded error, logs `weight_commit_refused` as an alert, and commits again at
a later tempo. A new validator with too little stake therefore keeps running
and starts committing once it has the stake. A `weight_commit_refused` alert
that recurs every tempo means the validator is not committing.

Any other refusal halts the validator with `recovery_requires_review` (exit 3,
the unit stays stopped), after it logs `finalized_failure_requires_review`.
Such a refusal can be a wrong weights version, a deregistered hotkey, a
disabled commit-reveal flag, a commit queue that never drains
(`TooManyUnrevealedCommits`) or no validator permit
(`NeuronNoValidatorPermit`). Several of these would fail every later tempo
too. A receipt that contradicts the journal, or one that cannot be read
(`receipt_unavailable`), also halts. Stop the unit and list the failures:

```sh
systemctl --user stop slop_ninja_validator-dispatch@my-instance
slop-ninja-journal --journal <journal_path> inspect
```

`review_required` names each failure's operation ID, attempt number, block and
decoded error (or `receipt_contradiction`, or `receipt_unavailable` with the
read's class). Fix the cause first, then record the review and start the unit
again:

```sh
slop-ninja-journal --journal <journal_path> review-failure --operation <operation_id> --attempt <n>
systemctl --user start slop_ninja_validator-dispatch@my-instance
```

The review is a permanent marker for that attempt. Nothing is deleted, and the
attempt stays in the journal as evidence. The tool refuses an attempt that is
still unresolved: run the validator first so recovery can classify it. You
can also review a failure listed under `awaiting_decode` (its receipt has not
been read) the same way.

A `receipt_contradiction` means the receipt reports success, or another
block, while the journal recorded a failure, so the commit may have landed.
`review-failure` refuses it (`receipt_contradiction`) unless you add
`--accept-contradiction`. Check the chain first (`slop_ninja_validator
observe`). The review is then recorded as a reviewed contradiction, counted
under `reviewed_contradictions` rather than `reviewed_failures`, and the
operation is retired as `receipt_contradiction_reviewed`. A landed timelocked
commit is revealed by the chain itself; the validator's reveal ledger does
not track it.

Reviewing a failure is roll-forward only. Every validator release before
`review-failure` existed, including earlier releases at the same rtao pin,
halts with `recovery_requires_review` on any finalized failure the journal
retains, reviewed or settled included, and cannot review it. Once your
journal holds one, do not install an older release; `rollback-check` reports
it and exits 3 (see [The launch burn row](#the-launch-burn-row)).

## Logs

Logs are structured JSON on stderr (or text). Events have fixed names and
counts and omit addresses, paths, ciphertext, signed bytes and endpoint paths.
`failure_receipt_unreadable` names only a fixed `class` of the failed read
(`timeout`, `transport`, `state_discarded`, ...); the read's error text, which
can carry endpoint detail, is logged only at debug level
(`failure_receipt_read_error`).
The dry-run and dispatch reports on stdout contain the row's entries, which are
public anyway.

Events with `"alert": true` need attention. Those about refused commits are
`weight_commit_refused` (the chain refused a commit, and the validator will
commit again; it names the decoded `error` and the `tempo`),
`failure_receipt_unreadable` (a refused commit's receipt could not be read;
signing is paused until it is, and `lasting` says whether it halts for
review. It is alerted once per run, and again only when its `class` changes) and `finalized_failure_requires_review` (the validator
halts; see [A rejected commit](#a-rejected-commit-recovery_requires_review)).
Each `recovery_round` event also counts `failures_awaiting_decode`,
`failures_awaiting_review`, `refusals_settled` and `reviewed_failures`.

## Reporting problems

Open an issue on the public repository's issue tracker (not yet live) with
the output of `slop_ninja_validator --version` (the version and the commit the
binary was built from; `slop-ninja-journal --version` prints the same), the
command, the exit code and the logged event names. Never post the journal,
keyfiles or mnemonics. Report a security problem privately to
[security@slop.ninja](mailto:security@slop.ninja) ([security policy](../../SECURITY.md)).

## Reference

- [Native validator README](../../subnet/crates/validator/README.md): the run
  order, configuration and tests in full.
- [Native chain adapter](../../subnet/README.md): the journal, runtime upgrades
  and the rtao pin.
- [Protocol crate](../../subnet/crates/protocol/README.md): records, packages
  and row derivation.
- The [whitepaper](../../whitepaper/slop_ninja.pdf): cohorts, lagged payout,
  rewards and the trust model.

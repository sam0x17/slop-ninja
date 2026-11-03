# Testnet validator guide (experimental)

This guide is for running the native validator `slop_ninja_validator` on
Bittensor testnet subnet 601. Testnet is announced for miners first; outside
validators are welcome to follow along, but their participation is not yet
supported end to end, and this guide is marked experimental. Testnet has no
monetary value; see the [terms](terms.md).

## What a validator does

Slop Ninja operates one lead validator, the coordinator. It runs the hidden
evaluation of every cohort, then signs and publishes a score package. Every
other validator:

1. fetches the published package from the public status host and verifies it
   against pinned public keys;
2. derives the native weight row itself, with the shared protocol crate and
   the frozen `reward-v1` rules;
3. commits that row on chain through its own journaled, timelocked
   commit-reveal, at every tempo while the cohort's target is active.

Validators work in parallel. None waits for the coordinator's own chain row,
and the coordinator's row and certificate are later consistency audits, not
prerequisites. The only thing other validators contribute is timely on-chain
submission and reveal of the rows they calculated.

### What a validator never receives

A validator never receives hidden evaluation data: not the held-out documents,
their labels, per-document predictions, the private probe, or any private model
weights. It trusts the coordinator's execution and labels, and checks the reward
calculation itself. No zero-knowledge proof or independent re-run of the hidden
evaluation is part of the protocol.

## What is live

| Piece | State |
| --- | --- |
| `slop_ninja_validator` startup checks, `observe`, the dry run and journaled dispatch | built and tested on Linux x86_64; live commits, per-tempo re-commits and fault drills ran on subnet 601 on October 30, 2026, under a fixture package |
| Verification of `reward-v1` packages | in the first signed release, not yet published |
| Published packages on the public status host | not yet live |
| The package fetcher (`[fetch]`) | built and tested offline against the publisher's layout; waits for the public host, which must also publish the current authorization (below) |
| Continuous dispatch, successor supersession and persistent user units | built and tested offline; not yet run unattended across cohorts on subnet 601 |
| Committing the launch burn row | built and tested offline (rtao's owner-row opt-in, below); not yet committed live on subnet 601 |
| Release public key | published on the [landing page](README.md#release-key) |
| Public repository | [sam0x17/slop-ninja](https://github.com/sam0x17/slop-ninja) |
| Signed release binaries (Linux x86_64) | built and signed; published with the public repository |

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

Release binaries are not yet live. Build from a checkout of the
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
are commented out: uncomment them when the public host is live and you want
to sign. Edited for subnet 601, the values that matter look like this (the
example's `[recovery]` and `[log]` sections keep their defaults):

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
  `keys.json` published beside a package. The keys in the repository's
  `subnet/coordinator/testnet-trust.json` today are rehearsal keys; they will
  be rotated before the testnet is announced, so use the file from the signed
  release.
- **`[package]`** names the bundle file the validator verifies. With
  `[fetch]`, the validator writes it there itself; without `[fetch]`, you
  install it. Without `[package]`, `run` stops after startup.
- **`[fetch]`** names the public status host (`https://` only, no
  credentials, query or fragment). The validator checks it at every start and
  then every `poll_secs` (300 by default, at least 60, the index documents'
  cache lifetime) during a dispatch run; see [Fetching published
  packages](#fetching-published-packages). `stall_secs` (60) abandons a
  request that receives nothing for that long; it resumes at the next check.
  The cache defaults to `public-cache` beside the journal.
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
   verified (below). Then verifies the package, derives the row and either
   prints the dry run or dispatches.

Without `--dispatch`, the journal is not written: the run derives the row,
encrypts it to the predicted reveal round, composes the timelocked commit,
type-checks it against live metadata and prints its signing-payload hash. No
key is read. With `--dispatch`, the row is enqueued as a journaled operation,
the keyfile is unlocked, and each finalized block drives the scheduler. Once
the commit is final the validator reads it back; once the reveal is observed
it compares the on-chain `Weights` row with the row that commitment carried.
The run then re-commits the target once per tempo, with the row derived
afresh from that tempo's finalized registrations, until it is stopped. A
successor target, once installed, takes over in the same process; a tempo
with nothing to commit (below) is waited out in the same process too.

| Exit | Meaning | Dispatch unit |
| --- | --- | --- |
| 0 | completed: the dry run printed; a bounded run reached its block bound or installed a successor (`superseded`); or a signal | restarts |
| 1 | an error, such as an RPC failure | restarts |
| 2 | invalid configuration, a missing journal, or `--dispatch` without `[package]` and `[dispatch]` | stays stopped |
| 3 | halted by a check: subnet or hotkey not registered, commit-reveal disabled, chain mismatch, recovery that needs review, a refused package or row, or chain weights that differ from the row of the commitment that set them | stays stopped |
| 75 | wait and retry: recovery still in progress, refused upstream (`upstream_storage_refused`), or work awaiting recovery; outside a continuous dispatch run, also a deferred tempo or no bundle installed yet (`bundle_unavailable`) | restarts |

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
`MemoryMax=1G` and `MemorySwapMax=0`. The dispatch unit is enabled for
`default.target`, and lingering starts your user manager at boot, so the
validator survives logout and reboots. The installer does all of this:

```sh
cd <checkout>/subnet/crates/validator
deploy/install.sh --instance my-instance --from <directory with the release binaries> \
  --config <your edited configuration> --init --enable --start
```

It installs the two binaries, the unit templates, the configuration with
mode 0600 (it never replaces
an existing one), and the state directory; runs `check-config`; creates the
journal with `--init`; enables lingering (`loginctl enable-linger`); and
enables and starts the dispatch unit. `--dry-run` prints each step instead.
It runs on Linux only. The release directory must hold `SHA256SUMS` listing
each of the two binaries exactly once, and both must match it;
`--unverified` skips this for binaries you built yourself. `SHA256SUMS` sits
beside the binaries, so it proves only that the download is intact: verify
the release's detached signature over it before you run the installer
above, with the key and allowed-signers line on the [landing
page](README.md#release-key):

```sh
cd <directory with the release binaries>
ssh-keygen -Y verify -f allowed_signers -I slop-ninja-testnet-release -n file \
  -s SHA256SUMS.sig < SHA256SUMS
```

Write `allowed_signers` from the landing page, not from the release's copy,
and stop unless `ssh-keygen` prints `Good "file" signature`. The release
binaries are Linux x86_64 (glibc 2.34 or later); elsewhere, build from
source and pass `--unverified`. Once the unit runs:

```sh
systemctl --user status slop_ninja_validator-dispatch@my-instance
journalctl --user -u slop_ninja_validator-dispatch@my-instance -f
```

Stopping the unit pauses the journal; retained work survives, and the next
start recovers first. Keep `recommit` at its default, `per_tempo`: with
`once`, every restart commits again.

## Fetching published packages

The fetcher reads the coordinator's signed records from the public status
host, in the publisher's layout `v1` (see [the public
layout](public-layout.md)):

- `v1/authorizations/current.json`: the authorization that lets validators
  commit now (`launch` or `cohort`) and any renewal of it, each a
  content-addressed object listed with its SHA-256 and its exact signed
  length. **The public host is not deployed yet, and the coordinator does
  not yet store the authorizations the publisher copies**; until both
  happen, the fetcher reports `unpublished` and the validator keeps the
  bundle you install;
- `v1/cohorts/index.json`: every published cohort, its target digest and
  whether its detailed receipts are still published;
- each cohort's `publication.json`, which lists every object with its SHA-256
  and length, and its `chunk-map.json` for the receipt bundles.

These documents are unsigned and only locate objects. Each object must match
its listed SHA-256 and length before it is used; the signed records are then
verified under your pinned keys exactly as for a bundle file you installed.
A lying host can make a fetch fail, never change what is verified. It also
cannot fill your disk or memory: an authorization's length is fixed by its
schema and its signature is checked before anything else is fetched; every
receipt bundle must be accounted for by the signed receipt index and listed
in `publication.json` before any is read; a session reads at most 64 MiB,
cached objects included, and ends after `poll_secs`; and every session
prunes the cache to what it and your installed bundle need. A refusal of
records the host served is logged as `fetch_refused` with `alert = true`.

For a cohort authorization, the fetcher takes the target's cohort and the
carrying cohort the authorization names (a later cohort that produced no
target), with their manifests, packages, closure, published index, receipt
index and every receipt bundle, assembles the bundle, and verifies it
against the target your validator holds. A session reads at most 64 MiB, the
bundle file bound, and refuses a declared length before reading the body. A
download interrupted part-way is kept as a partial file and continued with a
range request at the next check; an object that fails its digest is deleted
and fetched again. Verified objects stay in the cache while the current
pointer or your installed bundle needs them. One session at a time uses the
cache: `fetch` exits 75 while the running unit holds it.

What the validator does with a verified bundle:

- **The same target with a longer lease** (a renewal, or a newer carrying
  cohort's authorization): it is installed at once, and a continuous run
  uses it from the next tempo. Operation IDs bind the target, not the
  authorization, so nothing already queued changes.
- **A successor target**: it is installed at the eligible boundary, once the
  finalized epoch reaches the successor's activation (the closing boundary of
  its cohort). While any commitment of the old target is in flight (signed
  and not yet settled by finalized recovery), the switch waits, and no new
  commitment of the old target is queued meanwhile; work that never signed
  is retired as `superseded`. The run continues on the new target in the
  same process. The old target's last commitment still reveals afterwards;
  its `Weights` are compared with that commitment's own row, which the
  validator records beside the journal (`<journal>.reveal-rows.json`).
- **A target that activates no later than the installed one** (a stale or
  rolled-back pointer) is never installed.
- **A successor whose records are not all on the host yet** (not yet
  listed, an object missing, receipts not published) never produces a
  bundle: the old target keeps its per-tempo commitments.
- **A validator that fell behind** by more than one target (offline across
  several cohorts) checks each intermediate target's records and closure,
  without receipts because it commits nothing for them, and moves its held
  target forward through them to the current one.

Receipts follow the N+2 rule: a cohort's receipt index, chunk map and
bundles are deleted from the host once two later cohorts are published and
its target is superseded. The current target's cohort stays published. A
carrying cohort produces no target, so its receipts are deleted two cohorts
later even while the current authorization still names it. New commitments
need retained receipts, so a validator that never fetched a cohort's
receipts before they were deleted reports `receipts_expired` and keeps its
installed bundle; one that fetched them earlier assembles from its own
verified cache. Commitments already submitted recover from the journal
without receipts.

When the authorization's lease ends with no renewal or successor fetched,
new commitments pause (`lease_paused`, logged with `alert = true`), while
reconciliation and reveal checks continue; the next fetched renewal, carrier
or successor resumes them. Without `[fetch]`, the run first stays until the
reveals of its last commitments have been compared, then waits for the next
epoch in the same process and reads the bundle file again.

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
on the host yet, 3 when the protocol refused them.

## What the validator verifies

Before any row exists, the validator verifies the bundle and refuses it (exit 3)
on any failure:

- each signed record is authenticated before it is decoded: the cohort manifest,
  the score package and the calculation target under the package key, and the
  receipt index under the receipt key;
- the package is bound to its manifest, the target to the package, and the
  receipt index to the package's receipt root and count;
- every stored receipt object matches its index entry;
- the package names this validator's chain genesis and netuid.

For `reward-v1` bundles (from the first signed release, not yet published)
it also checks every receipt chunk, the published-artifact index against the
manifest's binding, the recomputed outcome, the cohort closure against the
manifest's minimums, and the reward-intent authorization against the active
target and its lease, at every tempo's re-commit as well.

It then reads the subnet's registrations (`SubnetworkN`, `Keys`,
`BlockAtRegistration`, `SubnetOwnerHotkey`) at the same finalized block and
derives the row. Credit belongs to a hotkey and its registration generation:
units of a hotkey no longer registered under that generation go to the owner
hotkey's UID, which burns them.

The validator does not check what it cannot see: the hidden documents, the
labels and the executions behind each score. It trusts the measured scores and
recomputes eligibility, clustering, credit, normalization and quantization
under the common versioned rules, so identical inputs give identical rows on
every validator.

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
silent. Commit-reveal hides each submission until its reveal, but the public
package lets anyone derive the same row, so it neither prevents copying nor
proves that a validator calculated the row itself.

Each active cohort target is re-derived and committed once per tempo, about 20
commitments a day, against a fresh finalized registration snapshot. During a
coordinator outage, validators keep re-committing the last active target within
its authorization lease; past the lease they pause new commitments
(`lease_paused`) until a renewal or successor is published. Cohorts are
delayed, never skipped.

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

A build older than the one that queued burn-row work cannot open that
journal, and in steady state the next tempo's burn row is almost always
queued, so the work never simply finishes. To roll back, stop the unit, run
`slop_ninja_validator --config <config> rollback-check --retire`, and install
the older build only when it exits 0 (see the validator README,
[Rolling back past this rtao pin](../../subnet/crates/validator/README.md#rolling-back-past-this-rtao-pin)).
**Not yet live:** this has not yet committed the burn row on subnet 601.

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

## Logs

Logs are structured JSON on stderr (or text). Events have fixed names and
counts and omit addresses, paths, ciphertext, signed bytes and endpoint paths.
The dry-run and dispatch reports on stdout contain the row's entries, which are
public anyway.

## Reporting problems

Open an issue on the public repository's issue tracker (not yet live) with
`slop_ninja_validator --version`, the command, the exit code and the logged
event names. Never post the journal, keyfiles or mnemonics. Report a security
problem privately; the contact will be on the [landing page](README.md).

## Reference

- [Native validator README](../../subnet/crates/validator/README.md): the run
  order, configuration and tests in full.
- [Native chain adapter](../../subnet/README.md): the journal, runtime upgrades
  and the rtao pin.
- [Protocol crate](../../subnet/crates/protocol/README.md): records, packages
  and row derivation.
- The [whitepaper](../../whitepaper/slop_ninja.pdf): cohorts, lagged payout,
  rewards and the trust model.

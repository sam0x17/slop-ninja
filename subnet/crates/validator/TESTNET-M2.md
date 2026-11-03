# Testnet M2 runbook: native weight commit on subnet 601

This runbook takes `validator2` from unregistered to a revealed, read-back
weight commit on Bittensor testnet subnet 601. The commit is driven by a
coordinator-signed fixture package (testnet shortcut T12, removed by M6) and
derived with the shared protocol crate's `testnet-reward-v0` (T8).

Who signs what:

- **You** sign every coldkey transaction (registration, stake, any owner
  hyperparameter) with the `slop-ninja-testnet` coldkey
  `5EEoBHwSVCJ2JqQtrPrZETXrak9GXSVJrLAyizW8E5A63Ur3`.
- **The validator** signs only timelocked weight commits, with `validator2`'s
  hotkey, and only under `run --dispatch`. The default `run` is a dry run that
  prepares the commit and prints its hash without loading a key.
- The fixture tool signs the package with **application** keys it generates
  locally. They are never chain keys.

The commands were checked offline (tests below), and the subnet facts come
from read-only RPC. Registration, stake and the default dry run (steps 2 to 5)
ran on 2026-10-30; the recorded dry run is in step 5. Live dispatch, per-tempo
re-commit and the fault drills ran the same day: every commit, block and
read-back is in step 6, the drills in step 7 and the measured RPC limits in
step 8. Evidence (logs, proxy traces, journal extracts, finalized read-backs)
is under `data/task-roots/testnet/m2-live/` on build-host.

## 0. Subnet 601 at preparation time

Read on 2026-10-30 04:37 UTC from `test.finney.opentensor.ai`, finalized block
8,332,729 (`0xd3645d54…e43e`); five read-only calls:

| Item | Value |
| --- | --- |
| Runtime | `node-subtensor` spec 486, transaction version 1 (observation only) |
| V15 metadata | byte-identical to `subnet/fixtures/runtime-486/testnet-486.v15.scale` (sha256 `582b1d05…1a81`) |
| `NetworksAdded(601)` | true |
| `SubnetOwner(601)` | `5EEoBH…63Ur3` (the coldkey above) |
| `SubnetOwnerHotkey(601)` | `5CAhKP3Qng8w2GbZWdvufyg7bfpHBGVGQ4fbZJsdqUmYFVKH` |
| `SubnetworkN(601)` | 1 (only UID 0, the owner hotkey) |
| `FirstEmissionBlockNumber(601)` | 8,331,494 (the subnet is started) |
| `CommitRevealWeightsEnabled(601)` | unset, so the default `true` applies |
| `CommitRevealWeightsVersion` | unset, default 4 (rtao supports 4) |
| `RevealPeriodEpochs(601)` | unset, default 1 |
| `Tempo(601)` | 360 |
| `WeightsSetRateLimit(601)` | unset, default 100 |
| `MinAllowedWeights(601)` | 1 |
| `SubnetEpochConsensus(601)` | unset, default `Yuma` (commit limit 5,000 bytes) |
| `MechanismCountCurrent(601)` | unset, default 1 |
| `Burn(601)` | 154,784,327 rao (about 0.155 τ per registration; it floats) |
| `MaxAllowedUids` / `ImmunityPeriod` / `ActivityCutoff` | 256 / 5,000 / 5,000 at this read. The owner then set `ImmunityPeriod(601)` to 21,600 at block 8,333,270 (2026-10-30), above the 25–49 hour first-payout lag; a read-only read on 2026-11-01 returned 21,600 |
| `Uids(601, coordinator / validator2 / miner1)` | none: not registered |

A later read-only query during review (2026-10-30, current finalized head)
found `SubnetTAO(601)` = 1,000,000,000 rao (1 τ), `SubnetAlphaIn(601)` about
1,955 α and `StakeThreshold` unset (default 0). Re-read the pool before
staking (step 2); it moves with every stake and registration.

**Step 0 result.** Every transaction M2 sends matches rtao `b5553a76`'s
generated calls and `docs/subnet-runtime-486-diff.md`: pallet and call index,
argument names and order, shape hash, and the bytes equal a by-name
re-encoding through live metadata. The checked calls are
`SubtensorModule.commit_timelocked_mechanism_weights` (7.118, the call rtao's
scheduler sends), `commit_timelocked_weights` (7.113), `burned_register` (7.7),
`add_stake` (7.2), `serve_axon` (7.4, only if a miner serves an axon),
`Commitments.set_commitment` (18.0, M5), and
`AdminUtils.sudo_set_commit_reveal_weights_enabled` (19.49) and
`sudo_set_commit_reveal_weights_interval` (19.57).
`set_mechanism_weights` (7.119) is tracked only so the guard can refuse it.
The test is `payload_tests::step0_generated_calls_match_live_486_by_name_index_and_type`,
and the golden vectors are in [`fixtures/payload-486.txt`](fixtures/payload-486.txt).
Under CRv4 there is no reveal transaction: the chain decrypts the commit at
`reveal_round` and applies it.

None of this is a runtime gate. If testnet upgrades before or during M2, the
validator keeps running: it resolves the calls by name in the new metadata and
defers only an operation with no type-correct encoding (`operation_deferred`
in the log, `alert = true` in the journal summary).

## 1. Build and install (build-host)

Build under the shared lock. The lock path is absolute (the worktree has no
`data/` directory), and `systemd-run --user` starts the unit in `$HOME`, so
the script changes directory itself:

```sh
T=/home/operator/workspace/slop_ninja/data/task-roots/testnet/targets/m2-release
flock /home/operator/workspace/slop_ninja/data/locks/cargo.lock \
  systemd-run --user --slice=slop-a-comp.slice \
  -p MemoryMax=12G -p MemorySwapMax=0 --wait --collect --pipe -- \
  bash -lc "cd /home/operator/workspace/slop_ninja/data/worktrees/subnet-testnet-m1/subnet && \
    CARGO_TARGET_DIR=$T cargo build --release --locked -j 4 \
    -p slop_ninja_validator -p slop-ninja-subnet"
install -m 0755 $T/release/slop_ninja_validator $T/release/slop_ninja_fixture_package \
  $T/release/slop-ninja-journal ~/.local/bin/
```

The drills (step 7) need a build with `--features testnet-drills` added to
the `slop_ninja_validator` package: it adds the `SLOP_NINJA_DRILL_FAULT`
crash hook and the `slop_ninja_drill_proxy` binary. Production builds omit it.
The M2 live runs used drill builds throughout, from
`m2-live/scripts/release.sh` into `m2-live/bin/<label>/`.

Build rtao's CLI at the same pin from the local read-only clone at
`b5553a76`, also under the lock. `--offline` uses the already fetched
dependencies, and nothing is written into the clone:

```sh
CT=/home/operator/workspace/slop_ninja/data/task-roots/testnet/targets/m2-rtao-cli
flock /home/operator/workspace/slop_ninja/data/locks/cargo.lock \
  systemd-run --user --slice=slop-a-comp.slice \
  -p MemoryMax=12G -p MemorySwapMax=0 --wait --collect --pipe -- \
  bash -lc "cd /home/operator/workspace/slop_ninja/data/task-roots/testnet/rtao-main && \
    git rev-parse HEAD && \
    CARGO_TARGET_DIR=$CT cargo build --locked --offline -j 4 -p rtao-cli"
install -m 0755 $CT/debug/rtao ~/.local/bin/
```

`git rev-parse HEAD` must print `b5553a76ff67f0f54350472ef0306f225e8a4c44`.

The rtao commands below follow rtao's book at that revision. Before you sign
anything, run each one with `--help` on the installed CLI and confirm the
arguments.

## 2. Register and stake (you sign with the coldkey)

Fund the coldkey first: at least 3 × the current burn plus the stake plus
fees. Read back the result at finality before the next step.

Registration (`burned_register`) must be MEV-shielded: rtao marks it
`mev_shield_required`, and `--no-mev-shield` is refused with
"burned_register must be submitted MEV-shielded" (exit 2). Keep the CLI
default, which shields it. The journaled-shield restriction elsewhere in this
runbook applies only to the validator's own weight commits, not to coldkey
transactions you sign with the CLI. Shielding is optional for `stake add`.

```sh
W="--network test --wallet slop-ninja-testnet"

# Current burn (registration cost) for 601.
rtao subnets burn-cost --network test --netuid 601

# Register validator2 and miner1 (shielded by default; do not pass
# --no-mev-shield). The coldkey pays the burn; the hotkey's pub.txt names the
# account. Coordinator registration is for M3 and optional now.
rtao subnets register $W --wallet-hotkey validator2 --netuid 601
rtao subnets register $W --wallet-hotkey miner1     --netuid 601
rtao subnets register $W --wallet-hotkey coordinator --netuid 601   # optional (M3)

# Stake to validator2. Re-read SubnetTAO(601) first and keep the amount small
# relative to it: the CLI's default slippage protection refuses a fill that
# moves the price more than 5% (`SlippageTooHigh`).
rtao stake add $W --wallet-hotkey validator2 --netuid 601 --amount-tao 0.02
```

With about 1 τ in the 601 pool, 0.02 τ stays within the default 5% tolerance;
10 τ would be ten times the reserve and fail with `SlippageTooHigh`.
`StakeThreshold` is 0, so any nonzero stake is enough for a Yuma validator
permit, and plan decision 13's 10 τ is not needed for M2. To stake more, choose
`--rate-tolerance <fraction>` or `--no-slippage-protection` deliberately and
expect a large price impact. Check each flag with `rtao stake add --help`.

Read back at finality and record each registration:

```sh
rtao subnets metagraph 601 --network test --json > /tmp/sn601-metagraph.json
```

Write down, for UID 0 (the owner hotkey `5CAhKP…FVKH`) and for miner1, the
UID, hotkey and registration block. The validator reads exactly
`Keys(601, uid)` and `BlockAtRegistration(601, uid)` at its finalized block.
The dry run in step 5 shows the row it derives without writing the journal, so
a wrong identity in the package is visible, and correctable with a new
package, before anything is enqueued or signed. A row that would credit only
the owner's UID 0 (for example after a mistyped registration block) is refused
as `row_would_be_held`.

Commit/reveal is already effective on 601 (the stored flag is unset and
defaults to `true`). If you want the owner setting stored explicitly, the
owner path is (preview, then submit without `--dry-run`; the admin freeze
window at the end of each tempo can refuse it):

```sh
rtao hparams set $W --netuid 601 --name commit_reveal_weights_enabled --value true --dry-run
rtao hparams get --network test --netuid 601 --name commit_reveal_weights_enabled
```

The validator checks the flag at startup (finalized state) and again before
every signature (best-block state). If it is ever disabled, new commitments
pause; nothing falls back to direct weights.

## 3. Build the T12 fixture package

Generate application signing seeds once (files are created `0600`; no seed is
printed). Keep them out of the repository:

```sh
install -d -m 0700 ~/.local/state/slop_ninja_validator/t12-keys
slop_ninja_fixture_package keygen --dir ~/.local/state/slop_ninja_validator/t12-keys
```

It prints the `[package.trusted_keys]` block for the validator configuration.
Then build the package for cohort 1 from the identities you recorded.
`--opened-block` is a finalized epoch-boundary block (any recent
`LastEpochBlock(601)`); it is recorded, not used for scheduling.

```sh
slop_ninja_fixture_package build --netuid 601 --cohort 1 --revision 1 \
  --opened-block <epoch boundary block> \
  --burn 0:5CAhKP3Qng8w2GbZWdvufyg7bfpHBGVGQ4fbZJsdqUmYFVKH:<UID 0 registration block> \
  --miner <miner1 uid>:5CaZiz9SiYwLpCyNNCiDrtPvCaMMSzdWa99zZPNqFxaLd6WR:<miner1 registration block> \
  --keys ~/.local/state/slop_ninja_validator/t12-keys \
  --out ~/.local/state/slop_ninja_validator/package-t12-c1.json
```

The tool verifies the bundle exactly as the validator will and prints its
digests. The synthetic cohort gives miner1 positive credit, so the row is
`[(miner1 uid, 65535)]`. A row whose only positive entries are the validator
itself or the owner's UID 0 would be held by rtao (`HoldPrevious`) and never
committed; the fixture avoids that.

## 4. Validator configuration for netuid 601

`~/.config/slop_ninja_validator/validator2.toml`:

```toml
network = "test"
netuid = 601
weight_version = 0          # WeightsVersionKey(601) is unset (0)
journal_path = "/home/operator/.local/state/slop_ninja_validator/validator2-journal.json"

[wallet]
path = "/home/operator/.bittensor/wallets"
name = "slop-ninja-testnet"
hotkey = "validator2"       # only validator2pub.txt is read outside --dispatch

[rpc]
endpoints = [
  "wss://test.finney.opentensor.ai:443",
  "wss://test.chain.opentensor.ai:443",
]
user_agent = "slop_ninja_validator/0.1 (+https://slop.ninja)"
max_requests_per_minute = 13        # read session
signing_requests_per_minute = 20    # separate signing transport, idle except at a commit
signing_connect_quiet_secs = 60     # the default: no read call for a minute either side of its connection
startup_timeout_secs = 60

[recovery]
max_rounds = 16

[log]
format = "json"
level = "info"

[package]
path = "/home/operator/.local/state/slop_ninja_validator/package-t12-c1.json"
[package.trusted_keys]
package = "<from keygen>"
receipt = "<from keygen>"
authorization = "<from keygen>"

[dispatch]
hotkey_file = "/home/operator/.bittensor/wallets/slop-ninja-testnet/hotkeys/validator2"
max_finalized_blocks = 100000
epoch_offset_blocks = 320   # commit late in each epoch (step 8)
recommit = "per_tempo"      # the default; "once" stops after the first reveal
```

`[dispatch]` is read only by `run --dispatch`. `epoch_offset_blocks` replaces
rtao's per-hotkey jitter with a fixed commit block after each epoch boundary.
On public RPC keep it late in the epoch (320 of 361 blocks): rtao's reveal
watcher then replays only the 25 to 45 blocks between the commit and its
reveal, at about 14 requests a block on a 15-per-minute budget, about 23 to
42 minutes (step 8; the same figures as the validator guide and README).
The hotkey keyfile must be unencrypted (as bittensor hotkeys normally are),
because the unit cannot answer a password prompt.

`signing_requests_per_minute` gives rtao's executor its own connection and
budget, which is also its burst. rtao allows a signature 30 s from nonce lease
to pool admission, and that window made 26 to 28 calls on testnet; behind a 13
to 15 per minute read budget it expires before signing (it did twice). Its
connection spends about 16 calls at once, so the run connects it last, after
every startup read, with `signing_connect_quiet_secs` on each side in which
the read session makes no call (step 8). Endpoints accept plaintext `ws://`
only to a loopback host, for the drill proxy (step 7), which connects onward
with TLS.

Check it offline and create the journal once:

```sh
C=~/.config/slop_ninja_validator/validator2.toml
chmod 0600 $C
slop_ninja_validator --config $C check-config
slop_ninja_validator --config $C init
```

## 5. Dry run

```sh
slop_ninja_validator --config $C run
```

This opens the journal (persisting pause), records live-metadata readiness,
recovers finalized history without a signer, checks subnet, registration and
commit/reveal at finality, resumes, verifies the package and its full receipt
index, reads the registration snapshot, derives the row, refuses a row rtao
would hold, and builds the unsigned commit. It does not enqueue anything or
otherwise change the journal's work, and it never opens the keyfile. Expect on
stdout:

```json
{ "outcome": "dry_run", "retained": false, "entries": [[<miner1 uid>, 65535]],
  "dry_run": { "signing_uid": <validator2 uid>, "reveal_round": …, "call_pallet_index": 7,
               "call_index": 118, "signing_payload_sha256": "…", "signed": false, … } }
```

and exit 0. Run it again: `operation_id` and `entries` are unchanged.
`row_digest` changes whenever the finalized head does, because the protocol
row binds the registration snapshot's block and digest; the operation ID
excludes the row, and a dispatch restart joins the operation as long as the
entries are the same. The call and signing-payload hashes differ on every run,
because timelock encryption is randomized. `retained` becomes
`true` only after a dispatch run has enqueued the operation. Only
`run --dispatch` enqueues the row as `WeightWork`. No tool removes enqueued
work, so dry-run every package before dispatching it.

Exit 3 with `"outcome": "refused"` names the failing check (for example
`receipt_object_mismatch`, `package_chain_mismatch`, `row_refused`,
`row_would_be_held`). Exit 3
from startup names `subnet_not_registered`, `hotkey_not_registered` or
`commit_reveal_disabled`. An error mentioning `row_not_preserved` means the
subnet's weight limits would change the derived row; nothing is signed.

### Recorded dry run (2026-10-30)

Subnet 601 at the time: UID 0 owner `5CAhKP…FVKH` (registered at 8,331,760),
UID 1 coordinator (8,333,066), UID 2 validator2 `5HGeGy…oqyRC` (8,333,072) and
UID 3 miner1 `5CaZiz…d6WR` (8,333,167). validator2 held enough
alpha (`TotalHotkeyAlpha`) for a validator permit and received it at the next
epoch (about block 8,333,320; first seen at 06:36 UTC), together with the
coordinator; before that only UID 0 held one.
The polling read was one `state_queryStorageAt` call every two minutes.

The package was built with `--cohort 1 --revision 1 --opened-block 8332959`
(`LastEpochBlock(601)`), `--burn 0:5CAhKP3Qng8w2GbZWdvufyg7bfpHBGVGQ4fbZJsdqUmYFVKH:8331760`
and `--miner 3:5CaZiz9SiYwLpCyNNCiDrtPvCaMMSzdWa99zZPNqFxaLd6WR:8333167`,
signed with locally generated application seeds (never chain keys): manifest
`d2c1e253…b325`, package `32aed011…b81a`, target `8c8b1b10…be68`, receipt root
`5eedbcce…1237` over 8 receipts, one scored miner.

The first dry run (finalized 8,333,338) exited 1 with `explicit resume
refused`: startup then recorded readiness after recovery, and that first
readiness write refused the adapter's resume. Startup now records readiness
before recovery. With that fix, each run took about three minutes under the
15-requests-per-minute budget and exited 0:

| | Existing journal | Fresh journal |
| --- | --- | --- |
| Finalized head | 8,333,389 | 8,333,416 |
| `operation_id` | `slopninja:weights:v1:255cda0a…926e` | same |
| `entries` | `[[3, 65535]]` | same |
| `row_digest` | `fc4ece62…fd23` | `9bb090eb…b6bd` |
| `signing_uid`, call | 2, `7.118` | same |
| Ciphertext bytes, `reveal_round` | 291, 33,514,364 | 291, 33,514,364 |
| `nonce`, `era_expiry` | 0, 8,333,526 | 0, 8,333,554 |
| `signing_payload_sha256` | `532cd1a9…0e44` | `dcbb0ca9…6092` |

Readiness had no deferral and no alert, and `retained` was `false`. The
journal afterwards held no operation and no attempt. No keyfile was opened and
nothing was signed or submitted. The fresh-journal run used a scratch journal
to exercise the first readiness write before resume.

## 6. Live dispatch

Run it as a durable user unit. The M2 runs used transient units in
`slop-a-comp.slice` (`MemoryMax=1G`, `MemorySwapMax=0`, `Restart=on-failure`,
`RestartPreventExitStatus=2 3`, `KillMode=mixed`, a wrapper that records each
exit status), started by `m2-live/scripts/start-run.sh`:

```sh
systemd-run --user --unit=slop-testnet-m2live-<name> --slice=slop-a-comp.slice \
  -p MemoryMax=1G -p MemorySwapMax=0 -p Restart=on-failure -p RestartSec=30 \
  -p 'RestartPreventExitStatus=2 3' -p KillMode=mixed -p UMask=0077 --collect -- \
  slop_ninja_validator --config $C run --dispatch
```

The installed template `deploy/slop_ninja_validator-dispatch@.service` is
equivalent. After startup and package verification the run enqueues the
target's first commit (`created = false` when an earlier run did),
re-authorizes every retained operation of the target with the verified row and
tracks the pending one (`target_adopted`), loads the hotkey, takes one
metagraph snapshot (`metagraph_snapshot`) and reads the epoch schedule
(`epoch_schedule`). It connects the signing transport last, between two quiet
minutes of the read session (`signing_transport_quiet` with `before_connect`,
`signing_transport_ready`, `signing_transport_quiet` with `after_connect`).
Then, for each finalized block, it checks that both RPC sessions are alive,
anchors rtao's timelock clock to the best head's `Timestamp.Now`, and calls
`dispatch_finalized`.

Events per commit: `attempt_recorded` (the operation's attempt count grew;
`finalized_success_block` once included), `reveal_pending`,
`commit_queue_read` and `commit_read_back` (this hotkey's entries in
`TimelockedWeightCommits(commit_epoch)`, read before the reveal drains it;
more than one halts the run as `duplicate_commit`), `tempo_enqueued` (the
re-commit for the next epoch), and after the reveal `reveal_completed` or
`weights_read_back` comparing `Weights(601, uid)` with the row (a mismatch
exits 3). With `recommit = "per_tempo"` the run continues until
`max_finalized_blocks`; with `"once"` it stops at `revealed`.

Independent read-back: `slop_ninja_validator --config <read-only config>
observe [--epochs n] [--at <block>]` reads, at finality (or at a recent
finalized block), the epoch state, this hotkey's commit-queue entries and
every hotkey's entry count, `Weights`, `LastUpdate` and `Incentive` at the
row's UIDs. It opens no journal or key. The M2 read-only config used 6
requests per minute.

### Recorded commits (2026-10-30)

Every commit was `SubtensorModule.commit_timelocked_mechanism_weights`
(7.118, CRv4, mechanism 0, version key 0), signed by validator2 (UID 2) with a
128-block mortal era, for package cohort 1 revision 1 with the row
`[(3, 65535)]` (miner1). The first commit uses the target's identity
`slopninja:weights:v1:255cda0a…926e`; each re-commit has its own
`slop_ninja:weights:tempo:v1:` identity for its epoch.

| Epoch | Operation | Extrinsic | Nonce | Era | Finalized in block | Reveal round | Queue read before reveal | Drill |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 7 | `slopninja:weights:v1:255cda0a…926e` | `0xa0b4389d…19dd` | 0 | 8,334,371–8,334,499 | 8,334,376 (`0x787f347e…0124`) | 33,517,241 | 1 entry (validator, at finality) | none |
| 8 | `…tempo:v1:54a55a97…1f4a` | `0x9d3efe5d…c53b` | 1 | 8,334,741–8,334,869 | 8,334,745 (`0x06d6aba0…26b4`) | 33,518,683 | 1 entry (`observe`, 8,334,760) | crash at `submitted` |
| 9 | `…tempo:v1:aaed50a5…6c95` | `0x58d9abef…5688` | 2 | 8,335,082–8,335,210 | 8,335,086 (`0x8ea2faee…b374`) | 33,520,119 | 1 entry (`observe --at 8335110`) | SIGKILL after ack |
| 10 | `…tempo:v1:a4646b5e…c488` | `0xb3c8fc06…b0af` | 3 | 8,335,448–8,335,576 | 8,335,453 (`0x29095d32…bdc2`) | 33,521,561 | 1 entry (`observe --at 8335470`) | 429 after send |
| 11 | `…tempo:v1:1b56d4f8…64e7` | `0x686af362…bb32` | 4 | 8,335,803–8,335,931 | 8,335,808 (`0xeeaf2715…62ae`) | 33,523,001 | 1 entry (validator at 8,335,818; `observe` at 8,335,826) | primary endpoint down after ack |
| 12 | `…tempo:v1:98cab907…1136` | `0xb12b3cab…5416` (withheld) | 5 | 8,336,165–8,336,293 | never included (`ExpiredUnexecuted`) | none | 0 entries (`observe --at 8336198`) | era expiry |
| 14 | same operation, second attempt | `0xfdb1fe19…1b56` | 5 | 8,336,879–8,337,007 | 8,336,884 (`0x87988b6c…7710`) | 33,527,321 | 1 entry (validator at 8,336,888; `observe` at 8,336,899) | re-sign after expiry |

In epochs 8–12 each queue also held one entry of another hotkey; validator2
never had more than one. Each of the five committed operations holds exactly
one signed attempt, finalized successfully. The epoch-12 operation's first
attempt expired unexecuted, and rtao signed it once more, with the same
nonce 5, in epoch 14. Nonces advance by one per executed commit, so nothing
else was signed. The next re-commit took label 15, the epoch after that
commit, so labels 13 and 14 were never used. At the end the journal held 7
operations and 7 attempts, with no unresolved attempt or finalized failure,
and the epoch-15 re-commit was pending.

**Weights equal the row.** After each reveal, `Weights(601, 2)` at finality
was `[[3, 65535]]`, exactly the package's u16 row: at 8,334,434 (epoch 8,
after commit 1's reveal; `LastUpdate(2)` = 8,334,376), 8,334,941 and
8,335,144 (`observe`), and at 8,335,918, where the validator itself logged
`reveal_completed` for commit epoch 11 with `weights_match = true`. The commit queue of each revealed epoch was empty afterwards.
`Incentive` at miner1's UID 3 was still 0 at 8,335,144; M2 is not judged by
incentive or emissions.

**The first live attempts did not sign.** Both failures below happened before
signing, so no attempt was recorded; the admission timeouts still consumed
their epoch's cadence slot:

1. rtao refused every attempt with "timelock clock has no verified time
   anchor": the adapter's `ChainClock` was never anchored (rtao anchors it in
   its own runtime's clock worker). Fixed in `a7597c33`.
2. Twice (blocks ~8,333,570 and ~8,333,697) rtao's 30 s admission window
   expired ("submission timed out without reaching the requested state")
   while its calls queued behind the read budget. Fixed by the separate
   signing transport (`8d306ca7`).

## 7. Fault drills

Drills run on the test network only, with a build that has the
`testnet-drills` feature (shortcut T13):

- `SLOP_NINJA_DRILL_FAULT=<phase>[:<n>]` crashes the process (exit 86, no
  pause, no destructors) at the nth arrival at a journal phase boundary:
  `journal_opened`, `runtime_surface_evaluated`, `recovery_round`,
  `commit_reveal_checked`, `resumed`, `package_verified`, `row_derived`,
  `enqueued`, `dispatched`, `submitted` (a dispatch call that added an attempt
  returned) and `tempo_enqueued`. It is refused on Finney, and builds without
  the feature refuse to start when it is set.
- `slop_ninja_drill_proxy` is a loopback WebSocket proxy to one public
  endpoint. It logs every submission's extrinsic hash, per-minute request
  counts and (with `--trace`) every request. It injects one fault around the
  first submission: `kill-after-ack` (SIGKILL the validator process in
  `--kill-unit` after the node acknowledged the submission),
  `refuse-after-ack` (drop the acknowledgement, close, answer HTTP 429 for
  `--refuse-secs`), `blackhole-after-send`, `down-after-ack` (drop the
  acknowledgement, close every connection, refuse all later ones) and
  `drop-before-send` (never forward it).

Every drill ended with a finalized read-back of the commit queue and the
journal: **no duplicate commit**, and each restart recovered from the journal.

| Drill | Epoch | What happened | Recovery |
| --- | --- | --- | --- |
| Crash at each startup and application phase (`journal_opened` … `dispatched`), first commit pending, 9 runs | 6 | Each run exited 86 at its phase. | Every restart opened paused, recovered, rejoined the same operation (`created = false`); 1 operation, 0 attempts after each (journal generation 49 → 77). |
| Crash at `submitted` | 8 | Exited 86 right after the commit's finality, before any read-back. | The next start (the `tempo_enqueued` drill) replayed the reveal in startup recovery (27 min) and resumed; the operation kept 1 attempt, handed off. |
| Crash at `tempo_enqueued` | 8–9 | Exited 86 right after queuing the epoch-9 re-commit. | The next start adopted it as pending (`target_adopted`, `pending = 1`) and committed it once (the SIGKILL drill below). |
| SIGKILL right after send | 9 | The proxy killed the process 0.11 s after the node acknowledged `0x58d9abef…`; exit 137, restart 30 s later. | Startup recovery found the bytes in finalized block 8,335,086 and handed them off without signing again (17.7 min round). |
| 429 after send | 10 | The node received `0xb3c8fc06…`; the acknowledgement was dropped and the reconnect got HTTP 429. rtao recorded the attempt `Unresolved` ("weight submission fate is unresolved"). | The loop ran signer-free recovery rounds until finalized block 8,335,453 classified the attempt, then resumed (`resumed_after_recovery`). |
| Endpoint failover | 11 | The node received `0x686af362…`; the proxy dropped the acknowledgement, closed both connections and refused every new one. | rtao reconnected through `wss://test.chain.opentensor.ai:443`, reconciled the lost watch and recorded the finalized inclusion (8,335,808) in the same call: no unresolved state, no second attempt. The same process logged `reveal_completed` (`weights_match = true`) after the reveal. |
| Era expiry | 12 | The proxy withheld the signed `0xb12b3cab…5416` (nonce 5, era 8,336,165–8,336,293) and closed the connection, so it never reached a pool. After its 26-minute deadline rtao recorded it `Unresolved`. | Signer-free recovery rounds scanned the era's 129 finalized blocks and classified it `ExpiredUnexecuted` at 17:31Z, then resumed. Epoch 12's queue at its last block (8,336,198) held no validator2 entry, and `LastUpdate(2)` stayed 8,335,808. Epoch 13 ended during recovery (rtao had reserved epoch 12's slot), so rtao signed the same operation once more in epoch 14: `0xfdb1fe19…`, same nonce 5, finalized at 8,336,884, one queue entry. That second attempt is not a duplicate commit. |

The 429 drill exposed an rtao behavior: a policy refusal (HTTP 403/429)
received while reconnecting marks the session permanently failed. The
signing transport never reconnected (rtao logged the stored 429 every 12 s
for 17 minutes), so its next commit would have failed. The dispatch loop now
checks both sessions before each block and exits with `transport_failed`, so
the unit restarts with fresh connections (`43e1ee4b`). The same drill showed
that the commit queue can drain before rtao's watcher reports the reveal; the
read-back now reads it before the reveal (`43e1ee4b`).

## 8. Measured limits on public RPC

Measured through the drill proxy on 2026-10-30 (validator read budget 13 per
minute, signing transport 20 per minute):

- **Startup** to `resumed` takes about 2–4 minutes with no reveal pending
  (connection about 1 minute). The first dispatch iteration followed about
  2 minutes later, and follows about 4 minutes later with the quiet minutes
  around the signing transport's connection (below).
- **An idle dispatch iteration** costs about 18 requests (about 1.5 minutes,
  7–8 finalized blocks coalesced) since the metagraph change below, and about
  35 (2.7 minutes, 13 blocks) before it.
- **rtao's metagraph worker** reads a block and the epoch schedule for every
  finalized block, about 15 requests per minute, more than the read budget.
  With it running, the reveal watcher replayed 10 blocks in 44 minutes
  (575 requests). The run now stops the worker after its startup snapshot
  (`431372f1`); rtao's scheduler only loads the graph.
- **rtao's reveal watcher** replays every finalized block while a commit
  awaits its reveal, at about 14 requests per block, about one block per
  minute against the chain's five. rtao makes no new attempt and startup
  recovery cannot finish until it has caught up. Committing at offset 320
  leaves 25–45 blocks between the commit and its reveal, a replay of about
  23–42 minutes at 15 requests per minute (27–48 at this run's 13), so one
  commit per tempo (361 blocks, about 72 minutes) fits.
  Committing at offset 20 would leave about 340 blocks to replay, several
  hours. A restart whose earlier process stopped between a commit and its
  reveal replays from the commit: 45 minutes in the re-run below.
- **rtao's admission window** (30 s from nonce lease to pool admission) made
  26 to 28 calls on the signing transport at each of the seven commits: 20 at
  once from its burst, then paced, reaching `author_submitAndWatchExtrinsic`
  after 18 to 24 s, followed by the watch's reads. Every commit minute
  therefore exceeded the 20 per minute guidance (table below). The window
  alone exceeds it, so this part needs an rtao change (NOTES item 8).
- **The signing transport's connection** spends about 16 calls at once from
  its burst (handshake, runtime, head subscriptions and rtao's `TokenSymbol`
  scan). Before `89b7a08a` it connected while the read session ran at its
  budget, so every one of the ten restarts reached 29 or 30 calls in its
  busiest 60 s (35 with a concurrent `observe`).
- **A failure before signing still costs an epoch:** rtao reserves the
  epoch's cadence slot before submitting, so an admission timeout waits for
  the next epoch.
- **Proving era expiry** took about 86 minutes from the withheld send: rtao's
  own reconcile waited out its 26-minute deadline, then recovery scanned the
  era's 129 blocks at about 6 requests per block (about 2 blocks per minute)
  and classified the attempt 60 minutes after the era ended.
- **After a long blocking call the finalized-head stream backfills every
  missed header** (`chain_getBlockHash` + `chain_getHeader`, about 2
  requests per block) before the loop sees a head: after the hour-long
  expiry call it walked about 400 blocks at the full budget. Restarting
  (about 3 minutes with no reveal pending) starts the stream at the current
  head.

Apart from the connection order (fixed below), these are rtao b5553a76
behaviors, reported upstream in
`data/task-roots/testnet/rtao-upstream/NOTES-FOR-RTAO.md` (items 7–13).

### Minutes over 20 calls (2026-10-30)

Counted from every request the proxies traced (`--trace`, 07:57Z to 18:33Z),
by UTC minute across all proxy logs (the pass proxy on 9955 and the drill
proxy on 9956 ran together during drills), with the busiest 60 s window of
each episode. The first proxy (07:33Z to 07:57Z) wrote only its rolling
per-minute records. Not measured: the two runs before 07:33Z, which connected
directly at a 15 per minute budget, and the failover drill's traffic to
`test.chain` after the fault.

22 traced UTC minutes exceeded 20 calls, and one untraced record did (07:36Z,
22). The proxies' rolling 60 s records, counted by the minute in which each
closes, give 20 such minutes; they straddle minute boundaries, so the peaks
differ (39 at 10:06Z, for example). The highest five-minute average was 22.4
calls per minute (16:04Z to 16:08Z, the expiry drill), 21.4 by the rolling
records. No upstream 429 was seen.

| Cause | UTC minutes over 20 (calls) | Busiest 60 s |
| --- | --- | --- |
| Restart: the signing transport connected while the read session ran at its budget (all ten restarts) | 08:20 (29), 08:31 (29), 11:17 (28), 11:55 (28), 12:05 (29), 12:46 (25), 12:47 (26), 13:20 (28), 14:27 (28), 15:42 (29), 18:01 (29) | 29–30; 35 at 12:46, with a concurrent `observe` |
| Commit: rtao's admission window on the signing transport, with the read session's calls (all seven commits) | 10:05 (37), 10:06 (22), 11:19 (35), 12:27 (34), 13:41 (35), 14:52 (29), 16:04 (32), 18:27 (32) | 36–47 (47 at the first commit, 10:05:42–10:06:42) |
| Expiry drill: the end of rtao's 26-minute lost-watch reconcile on the reconnected signing transport (20 per minute) as the read session resumed | 16:30 (21) | 23 |
| `observe` read-backs (6 per minute) beside a running validator (13 per minute) | 12:39 (21), 18:30 (21) | 22, also at 10:17, 12:44, 13:55, 16:52 and 16:55 |
| The superseded submission route (first run, before `8d306ca7`), untraced | 07:36 (22, rolling record) | not traced |

The rolling records put the same causes at 07:36, 08:21, 08:31, 10:06 (39),
11:18, 11:20 (33), 11:56, 12:06, 12:28 (35), 12:47 (34), 13:20, 13:41 (30),
14:28, 14:52 (35), 15:43, 16:04 (27), 16:30 (22), 16:52 (21), 18:02 and
18:28 (30); each restart minute there is 28 or 29 (34 at 12:47, with the
`observe`).

**Restart fix (`89b7a08a`).** Lowering the signing transport's burst would
not help: its connection calls still fall in one minute, and the admission
window needs the burst. The run now adopts its target (journal only), loads
the hotkey, takes the metagraph snapshot, reads the epoch schedule, and only
then connects the signing transport, with `signing_connect_quiet_secs` (60 s)
before and after it in which the read session makes no call. A restart takes
about two minutes longer.

Re-run on 601 (2026-10-30, 19:08Z to 20:03Z) with a build of `89b7a08a`
without the drill feature, through the pass proxy with `--trace`
(`m2-live/runs/proxy-quiet1.log`, `m2-live/evidence/rpc-quiet1-minutes.jsonl`). Its
configuration was the M2 one with `max_finalized_blocks = 3` and
`epoch_offset_blocks = 50400`, so rtao never found a commit due and nothing
was signed. The epoch-14 commit's reveal was still pending in the journal, so
startup recovery replayed it for 45 minutes at 13 calls per minute. The read
session's last call came 60.7 s before the signing transport connected and
its next 60.1 s after; the connection made 15 calls in 2.1 s (19:57:47Z). No
minute exceeded 20 calls: at most 15 in a UTC minute (19:57Z, the connection
alone) and 16 in any 60 s window (the read session's own connection at
19:08Z); the proxy's rolling records peaked at 15. The run made three
dispatch iterations and exited 0. The journal still holds 7 operations and 7
attempts, none unresolved, paused, with the label-15 re-commit pending.

Still over 20 per minute: each commit's minute (rtao's admission window,
NOTES item 8); rtao's own traffic on the signing transport after a fault (the
expiry drill's reconnect and 26-minute reconcile ran at its 20 per minute
budget beside the read session); and an `observe` run beside a validator on
the same address. Run `observe` only while the validator is stopped, or give
the two budgets a sum of about 20.

## 9. After M2

- Stop the dispatch unit. The journal and the seed directory are evidence;
  back them up with the wallet and do not publish them.
- Per-tempo re-commit (plan decision 12) is implemented: the run re-commits
  its verified target once per tempo, deriving each tempo's row afresh. A
  continuous run installs a fetched successor once the old target's pending
  commitment has settled and continues on it in the same process
  ([README](README.md#fetcher)); with bundles the operator installs (T12),
  a successor still needs a new run.
- `Incentive` for miner1 was still 0 three epochs after the first reveal.
  Check it against the coordinator's own weights before M3.

## 10. Persistent unit (2026-11-01)

validator2 moved from a transient drill unit to the persistent user unit
`slop_ninja_validator-dispatch@<instance>` (main `c5a5d414`, installed with
`deploy/install.sh`, enabled for `default.target` with lingering; continuous
dispatch, `epoch_offset_blocks = 320`, the same journal and the operator-installed
`reward-v1` bundle of cohort 8's target carried by cohort 10, lease epochs
53 to 61; no `[fetch]`, because the public host is not deployed).

- The transient unit was stopped (SIGTERM, `interrupted`, journal paused)
  after its epoch-56 reveal read back `weights_match: true`. The new unit's
  startup took about 3.5 minutes with no reveal pending, joined the
  epoch-57 operation the earlier binary had enqueued, recorded it in the
  reveal ledger and bound the held-target record to the trust roots
  (`held_target_bound`).
- Its epoch-57 commit finalized at block 8352367. The unit was then
  restarted (`systemctl --user restart`) between that commit and its reveal.
  The new process's startup recovery replayed for 20 minutes, its dispatch
  loop reached the reveal 28 minutes later, and the reveal, made by the
  earlier process's commit, was attributed through the ledger and compared
  with that operation's own row at the reveal block (8352404):
  `reveal_completed`, `weights_match: true`. The unit did not restart on its
  own (`NRestarts` 0).
- The epoch-58 and epoch-59 commits (blocks 8352726 and 8353084) revealed
  at 8352765 and 8353125, both `weights_match: true`, with no restart. Each
  reveal was compared about 45 to 50 minutes after its commit: the dispatch
  loop waits on rtao's reveal replay on the public endpoint's budget.
- The unit was upgraded to main `ae5cf772` (the review round-2 fixes) by
  `systemctl --user stop` after the epoch-59 reveal, `deploy/install.sh
  --from <release directory>` (both binaries checked against `SHA256SUMS`)
  and `systemctl --user start`. Startup took 3.2 minutes with no reveal
  pending and joined the epoch-60 operation the earlier binary had enqueued.
  Its commit finalized at 8353449 and revealed at 8353484,
  `weights_match: true`.
- Memory: the unit's peak stayed between 56 and 59 MB (MemoryMax 1G) with
  this 15,344-receipt bundle.
- Lease end (lease epochs 53 to 61, no renewal staged). The tempo-61
  operation's first attempt timed out on submission (`weight attempt
  failed`, recorded unresolved). Signer-free recovery rounds, about 20
  minutes apart on the public budget, classified it expired after its era
  about an hour later, and rtao signed the same operation again in epoch 62
  (block 8354169), as in the era-expiry drill above. The next tempo was then
  refused (`authorization_refused`): the run logged `lease_paused`
  (`awaiting_reveals: true`, `fetcher: false`, `alert: true`) and stayed in
  the dispatch loop until that commitment's reveal (8354204) compared,
  `weights_match: true`, and only then left for the per-epoch wait
  (`tempo_deferred`, `tempo_waiting`, `deferral_wait`), re-reading the bundle
  each epoch. The unit stayed active with `NRestarts` 0.
- In all, five reveals under the persistent unit (commit epochs 57 to 60, and
  the tempo-61 operation committed in epoch 62) read back
  `weights_match: true`, across two unit restarts. No cohort boundary was
  crossed: no new cohort was opened, and nothing publishes the authorization
  pointer the fetcher reads yet.

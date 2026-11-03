# Testnet coordinator runbook

This runbook covers the deployed testnet coordinator for Slop Ninja subnet 601
on Bittensor testnet (test.finney). From M3 the coordinator holds the
coordinator hotkey (`5Hj59hQjQKVUp2NgzvZeNUQX8xoPotuqLGtg87UvNzSjFdRm`, UID 1)
as a Worker secret and makes one kind of chain write: its own timelocked
weight commit, once per subnet epoch. All its chain access is RPC to
`https://test.finney.opentensor.ai`. See [README.md](README.md) for the design.

Account IDs, subscription IDs and secret values are kept out of this file. The
operator's private inventory records them, together with creation times and
teardown commands. It and the secret files (directory mode 700, files
mode 600) are kept outside the repository.

## Resources

Every name carries the `-testnet` marker.

| Resource | Kind | Notes |
| --- | --- | --- |
| `slop-ninja-coordinator-testnet` | Worker on workers.dev | URL `$COORDINATOR_URL`, recorded in the private inventory. No preview URLs, observability off, deploy tag `testnet`, var `NETWORK="testnet"` |
| cron `* * * * *` | Workers cron trigger | The watchdog, and the only timer |
| `slop-ninja-coordinator-testnet_ChainJournal` | Durable Object namespace (SQLite) | Binding `CHAIN_JOURNAL_TESTNET` |
| `slop-ninja-coordinator-testnet_EvalCohort` | Durable Object namespace (SQLite) | Binding `EVAL_COHORT_TESTNET` (stub) |
| `receipts-bucket-testnet` | R2 bucket, Standard | Private: the r2.dev URL is disabled, and there is no custom domain or CORS rule. Binding `RECEIPTS_TESTNET`; unused so far |
| `eval-bucket-testnet` | R2 bucket, Standard | Private, as above. Binding `EVAL_TESTNET`; unused so far |
| `coordinator-subscription-testnet` | rtao-rpc testnet webhook subscription | Stream `finalized`, key `pilot-v1`, delivering to `/hook` |

Worker secrets: `COORDINATOR_STATUS_TOKEN`, `COORDINATOR_OPERATOR_TOKEN`,
`RTAO_WEBHOOK_SIGNING_SECRET`, `RTAO_WEBHOOK_SUBSCRIPTION_ID` and, from M3,
`COORDINATOR_HOTKEY_SEED`.

No Queues, Containers, custom domains, routes, KV, D1 or API tokens exist.

## Conventions

Use the operator's existing Wrangler OAuth login. Never run `wrangler login` or
`logout`, never create API tokens, and never print credentials. Every command
selects the operator's Cloudflare account and turns Wrangler's log files off:

```sh
export CLOUDFLARE_ACCOUNT_ID="$(cat /path/to/private-secrets/account_id)"
export WRANGLER_WRITE_LOGS=false WRANGLER_SEND_METRICS=false
export COORDINATOR_URL="<the coordinator Worker URL, from the private inventory>"
cd subnet/coordinator
```

Secrets reach Wrangler on stdin only, never through argv. The same rule applies
to the bearer tokens and the provider's management password, which go to curl
as `--header @-`.

## Create the buckets

```sh
wrangler r2 bucket create receipts-bucket-testnet
wrangler r2 bucket create eval-bucket-testnet
for b in receipts-bucket-testnet eval-bucket-testnet; do
  wrangler r2 bucket dev-url get "$b"      # expect: public access ... is disabled
  wrangler r2 bucket domain list "$b"      # expect: no custom domains
done
```

## Deploy

Wrangler's `[build]` command runs `worker-build --release`, which compiles Rust,
so run the deploy in a bounded build job (explicit memory limit, no swap,
`CARGO_BUILD_JOBS=4`) that runs:

```sh
wrangler deploy --env testnet --tag testnet \
  --message "slop-ninja coordinator testnet $(git rev-parse --short HEAD)"
```

The output lists the `-testnet` bindings and `schedule: * * * * *`. Cloudflare
can take several minutes to start a new cron trigger. On 2026-10-30 the first
tick ran about 8.5 minutes after the first deploy.

## Secrets

```sh
umask 077; S=/path/to/private-secrets
openssl rand -hex 32 | tr -d '\n' > $S/status_token
openssl rand -hex 32 | tr -d '\n' > $S/operator_token
wrangler secret put COORDINATOR_STATUS_TOKEN   --env testnet < $S/status_token
wrangler secret put COORDINATOR_OPERATOR_TOKEN --env testnet < $S/operator_token
```

## Coordinator hotkey (M3)

The hotkey file is a plaintext wallet keyfile. Its `secretSeed` is piped to
Wrangler on stdin and never echoed, written to argv or copied
(the operator's private upload helper checks the file's
SS58 address first):

```sh
python3 -I -c 'import json,sys; sys.stdout.write(json.load(open(HOTKEY_FILE))["secretSeed"])' |
  wrangler secret put COORDINATOR_HOTKEY_SEED --env testnet
```

Then install the T12 target and resume signing (it starts paused):

```sh
printf 'Authorization: Bearer %s\n' "$(cat $S/operator_token)" |
  curl -sS --header @- -H 'Content-Type: application/json' \
    --data-binary @package-t12-c1.json "$U/signing/target"
printf 'Authorization: Bearer %s\n' "$(cat $S/operator_token)" |
  curl -sS --header @- -H 'Content-Type: application/json' \
    -d '{"action":"resume"}' "$U/signing/control"
```

`{"action":"pause"}` stops new commitments; existing attempts keep
reconciling. `{"action":"arm_drill","drill":NAME,"count":1}` arms a testnet
drill (README, "Pauses, faults and drills"). Removing the secret pauses signing
with `hotkey_secret_unavailable`.

## Webhook subscription

Follow the provider's private testnet webhook instructions. The
management helper loads `RTAO_WEBHOOK_BASE` and `RTAO_WEBHOOK_PASSWORD` from
the private env file and sends the password on stdin:

```sh
hook_api() {   # as in the provider instructions
  printf 'Authorization: Bearer %s\n' "$RTAO_WEBHOOK_PASSWORD" |
    curl --fail-with-body --silent --show-error --user-agent 'slopninja-webhooks/1.0' \
      --header @- --header 'Content-Type: application/json' "$@"
}
umask 077; source <private client env file>
hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions" \
  --data "{\"name\":\"coordinator-subscription-testnet\",\"url\":\"$COORDINATOR_URL/hook\",\"streams\":\"finalized\"}" \
  --output $S/registration.json          # starts paused; never print this file
```

Extract `signing_secret` and `subscription.id` into mode-600 files without
printing them. Then install both:

```sh
wrangler secret put RTAO_WEBHOOK_SIGNING_SECRET  --env testnet < $S/rtao_signing_secret
wrangler secret put RTAO_WEBHOOK_SUBSCRIPTION_ID --env testnet < $S/rtao_subscription_id
hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet/verify"
hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet/resume"
```

A successful `verify` also activates delivery (`paused: false`). The `resume`
that follows is then a no-op.

## Operate

```sh
# status (content-free JSON; 401 without the token)
printf 'Authorization: Bearer %s\n' "$(cat $S/status_token)" |
  curl -sS --header @- $COORDINATOR_URL/status

# clear a durable fault (operator token), optionally re-anchoring the cursor
printf 'Authorization: Bearer %s\n' "$(cat $S/operator_token)" |
  curl -sS --header @- -H 'Content-Type: application/json' \
    -d '{"fault_id": N, "reanchor": false}' \
    $COORDINATOR_URL/fault/clear

# pause or resume provider delivery (the watchdog takes over after 300 s)
hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet/pause"
hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet/resume"

# invocation outcomes, best effort and sampled (keep the output private: it includes request headers)
wrangler tail --env testnet --format json
```

### ChainJournal unresponsive

Every delivery, cron tick, signing step and status read goes through the one
ChainJournal instance. `/status` answers within about 10 s even when that
instance hangs, and reports `chain_journal_liveness.verdict`, from the cron
watchdog's timed `/tick` call recorded outside the journal (README,
"ChainJournal liveness"):

- `ok` or `slow`: the instance answered the last tick.
- `unresponsive`: the last tick failed or timed out after 10 s;
  `record.consecutive_failures` counts the minutes, and `chain_journal` is
  `null` when the status read hung too.
- `stale`: no tick for 3 minutes, so the cron itself is not running (after a
  deploy Cloudflare can take several minutes to start it).

Each Worker reports its own ChainJournal: the coordinator's record is
`ops/chain-journal-liveness.json` and the evaluation Worker's is
`ops/eval/chain-journal-liveness.json`, both in `eval-bucket-testnet`, so
the evaluation Worker's ticks never touch the coordinator's verdict.

The operator's private status script prints the verdict and the signing
summary. When the verdict stays `unresponsive` for more than two minutes:

1. Pause provider delivery, so the restart happens with little traffic (a
   redeploy under steady load did not restart a wedged instance on
   2026-10-30, and one with little traffic did):
   `hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet/pause"`.
2. Stop any status polling, then redeploy the current commit (Deploy, above).
   Each start of the instance raises the counter `chain_journal_starts`.
3. Wait for `/status` to answer with the verdict `ok` (the next tick after the
   cron restarts). Signing recovers on its own: a submission marked before the
   wedge becomes `uncertain` (`abandoned_after_mark`) and is reconciled from
   finalized state; nothing is resent.
4. Resume provider delivery:
   `hook_api --request POST "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet/resume"`.
   Do this also when the provider paused the subscription on its own after
   repeated failed deliveries (its retry schedule is 1 s, 5 s, 30 s, 5 min and
   30 min, then a pause); `GET "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet"`
   shows `paused`. The watchdog covers the silence in fallback meanwhile, and
   the backfill closes the gap after the resume.
5. Record the window and the restart versions in the private inventory.

### Alerts

The publisher evaluates the alert rules every minute and posts what is due to
the private alerts relay Worker, which also watches the publisher's heartbeat
and the spend tracker. The relay writes every alert, once per event id, as a
content-free object to the private alert feed in its bucket (`feed/v1/`), and
rewrites `feed/v1/latest.json` every tick as its heartbeat. The operator reads
the feed with private tooling, which raises `relay_silent` itself when the
relay's writes stop. Email is an optional relay path, off by default. A status
read that does not answer, or answers without a field a rule reads, is never
read as a cleared condition, in the publisher's rules or in the operator's
`relay_silent`: the rules it decides stay as they were until a read answers. Rules
and bounds: `subnet/alerts-relay/README.md` and the publisher's README
("Alerts").

## Teardown

```sh
hook_api --request DELETE "$RTAO_WEBHOOK_BASE/v1/subscriptions/coordinator-subscription-testnet"
wrangler delete --env testnet      # Worker, cron, secrets, both DO namespaces and their data
wrangler r2 bucket delete receipts-bucket-testnet   # buckets must be empty
wrangler r2 bucket delete eval-bucket-testnet
```

## Measured on 2026-10-30

These figures come from the Worker built from commit `b9fd0ac3`, deployed
04:02:54 UTC. Measurements came from `/status` polled every 3 s, `wrangler tail`
and a small number of read-only RPC calls from `build-host`.

### Primary mode (04:05 to 04:20 UTC, 15 min)

**Deliveries.** The coordinator accepted 83 deliveries: 81 blocks applied by
webhook and 1 covered by the RPC anchor. Every tailed `/hook` invocation returned
204, none returned 401, and there were no exceptions. There were no provider
retries and no duplicate deliveries.

**Cursor.** The cursor advanced monotonically from 8332569 to 8332644. Its lag
behind the notified head was at most 1 block. It ran 0 or 1 block behind the RPC
finalized head at each of 10 watchdog ticks, and all 10 head audits passed. A
separate RPC `chain_getFinalizedHead` call from `build-host` returned exactly the
cursor hash, at block 8332599, when the cursor was 7 s old.

**Receipt timing.** The median gap between receipts was 12.3 s, ranging from 6.2
to 18.8 s.

**Latency** from block timestamp (`Timestamp.Now` read at 6 sampled blocks) to
the coordinator's durable receipt was a median of 33.3 s, ranging from 27.4 to
33.5 s. That figure includes GRANDPA finality, the provider's publish and
delivery polls (3 s each) and network time.

**Cost per invocation.** `/hook` CPU had a p50 of 1 ms and a maximum of 12 ms,
with wall time p50 28 ms and maximum 136 ms. Cron ticks used 13 to 35 ms of CPU
and 0.4 to 2.7 s of wall time.

**RPC use.** The Worker made at most 2 calls a minute: one head read per tick
and none per delivery.

### Fallback drill

The silence threshold stayed at its default of 300 s throughout. All times are
UTC.

| Time | Event |
| --- | --- |
| 04:21:57.5 | Subscription paused through the management API; the last delivery was accepted at 04:21:57.6 |
| 04:22-04:26 | Five ticks stay `primary`. Watchdog lag grows 1 → 21 blocks, below the 25-block `behind` threshold |
| 04:27:13.9 | The tick 316 s after the last delivery enters `fallback` (`webhook_silent`). Within about 4 s, its RPC walk applies 26 blocks, 8332652 → 8332678, so the lag is 0 |
| 04:28, 04:29, 04:30 | Each fallback tick reads the head and walks the cursor to it within about 3 s, 5 blocks per tick, using 3 RPC calls a minute |
| 04:30:38.2 | Subscription resumed |
| 04:30:38.8 | The first resumed delivery returns the coordinator to `primary` (`webhook_resumed`), with the cursor at 8332693 |
| 04:30:38-48 | The provider drains its backlog: 44 deliveries in about 10 s. 41 are deduplicated as `duplicate_blocks_webhook` (blocks 8332652-8332692, already applied from RPC). The other 3 are new blocks applied by webhook |

After the drill, the retained finalized chain had 140 blocks, exactly the
heights from the anchor at 8332563 to the cursor at 8332702. Every block was
applied once, either by webhook (98) or by RPC (41). There were no gaps, no
conflicts and no faults, and the claim fenced 4 generations. Both transitions
are in `mode_transitions` and show on `/status`.

### Observations

- **First cron tick.** The first tick came about 8.5 minutes after the first
  deploy, which is cron propagation. Until then the coordinator had only the
  webhook. Its first delivery anchored the cursor through RPC.
- **`mode_age_ms` on a new journal.** Before the first transition, `/status`
  reports `mode_age_ms` as milliseconds since the Unix epoch, because
  `mode_since_ms` starts at 0. This is cosmetic.
- **`head_number` in transitions.** `mode_transitions.head_number` is null for
  the `webhook_silent` and `webhook_resumed` transitions.
- **Tail sampling.** `wrangler tail` captured only some of the invocations, so
  delivery counts come from the journal counters.

### RPC-confirmed watermark (redeploy, 04:52 UTC)

This redeploy is the Worker built from commit `220422fc`, deployed at
04:52:31 UTC. Before it, an authenticated delivery advanced the cursor that
scheduling read, with only a later RPC head audit as an independent check.
Schema v3 now keeps webhook-advanced blocks provisional and adds a watermark
that only RPC evidence raises (README, "RPC use and trust").

- **Migration.** The live journal migrated in place from v2 to v3 on its first
  request. It kept 243 retained blocks, the cursor and all counters. The
  watermark was seeded at the anchor, 8332563, which left 243 blocks
  provisional.
- **First tick** (04:53:14 UTC). Its head read returned 8332808, which matched
  the retained hash. That one audit confirmed the whole retained chain, 245
  webhook and RPC blocks, with no extra RPC calls.
- **Steady state** (04:53 to 04:57 UTC, 5 ticks, `/status` polled every
  20 s). Each tick raised the watermark, with basis `rpc_head`. It trailed the
  cursor by 1 to 5 blocks, under one tick of blocks. Audits passed rose from
  42 to 46. Each tick made its 2 head-read calls and deliveries made none.
  The shared budget window once held 4 calls, from two ticks inside one 60 s
  window, against a limit of 12. There were no faults.

`/status` now reports `journal.confirmed`, `confirmed_basis`,
`confirmed_age_ms` and `provisional_blocks`.

## M3 signing, measured on 2026-10-30

All times UTC; blocks are testnet heights. Deploys, by commit: 2282604f
(10:50:01), 0a832755 (11:02:13), 5314c31e (11:05:30), 04ec8f33 (11:09:06),
40924a90 twice (12:53:46 and 12:59:28, restarts after the outage below) and
c196bfa9 (13:02:01, the current version). Each deploy restarted
the Durable Objects and the signing journal carried over; three of them
happened while the epoch-8 commit awaited its reveal. Signing stays active:
the coordinator keeps committing the T12 target once per epoch.

### Deploy and the first commit (epoch 8)

| Time | Event |
| --- | --- |
| 10:50:01 | Deploy. The live journal migrated v3 → v4 in place on its first request. The paused signer read the runtime version (486/1) and the V15 metadata at block 8334588; its SHA-256, `582b1d05…1a81`, equals the recorded fixture |
| 10:50:31 | `COORDINATOR_HOTKEY_SEED` installed from the wallet file on stdin |
| 10:50:35 | T12 bundle installed (SHA-256 `0615dc90…45cb9`, the native validator's file); target `8c8b1b10…62be68` |
| 10:50:41 | Resume. The preflight at confirmed block 8334593 created the epoch-8 operation `…11e6a040c461`, read the registrations of all 4 UIDs and derived the row `[(3, 65535)]` with testnet-reward-v0 (UID 3 is the allow-listed miner; the same row validator2 commits). Signing then paused with `hotkey_secret_unavailable`: that DO instance predated the secret's version |
| 10:51:42 | Resume. Preflight at 8334603 (nonce 0); signed with era 8334603 to 8334667, drand round 33518679 and predicted reveal block 8334759; marked; submitted; the node returned `0x3a6bcaae…f2a5f8a0`, equal to the journal's hash |
| about 10:52:30 | At confirmed block 8334608 the nonce was 1. The binary search found block 8334607; its extrinsic 6 is exactly the journaled bytes; its events hold `ExtrinsicSuccess` and `TimelockedWeightsCommitted`. Read-back of `TimelockedWeightCommits(601, 8)` at 8334608: `ok` |
| 11:24:37 | The reveal check was due at confirmed block 8334761, but the watermark moved from 8334758 to 8334763 (it advances about five blocks at a time, with each cron head read). The first check, at 8334763, still found the entry queued and set the next check for 8334768 |
| about 11:25:30 | The check at 8334768 found the entry gone. The queue search over 8334758 to 8334768 found block 8334764 (five blocks after the prediction: the drand pulse arrived late), whose events hold `TimelockedWeightsRevealed` by the coordinator, and `Weights(601, 1)` there equals the row: `revealed`, `weights_match_row` |

The journal did not log reveal checks then; it does now (`reveal_check`,
`reveal_probe`). The sequence above follows from the watermark history in the
status poll log and a chain read from `build-host` (4 calls): the
coordinator's entry is in `TimelockedWeightCommits(601, 8)` at 8334763 (683
bytes, two entries) and absent at 8334764 (342 bytes, one entry). An earlier
version of this runbook said the check at 8334761 found the entry gone, which
the chain contradicts.

The first live run used an eight-block event scan for the reveal; it was
replaced by the queue search (5314c31e) before the reveal, because a late pulse
can exceed any fixed scan.

**Independent read-back.** From `build-host`, through the other public endpoint
`test.chain.opentensor.ai` (8 calls in all):

- block 8334607 is `0x9b97c903…da4f80c3`; its extrinsic 6 has hash
  `0x3a6bcaae…f2a5f8a0`, signer `5Hj59hQj…`, nonce 0 and call
  `SubtensorModule.commit_timelocked_mechanism_weights`;
- at the finalized head after inclusion, `TimelockedWeightCommits(601, 8)`
  held the coordinator's entry (block 8334607, a 291-byte ciphertext, round
  33518679), the hotkey's nonce was 1 and `SubnetEpochIndex(601)` was 8;
- block 8334764 (`0x5bd37c13…c3ff898c`) holds `WeightsSet(601, 1)` and
  `TimelockedWeightsRevealed(601, 5Hj59hQj…)` in its initialization events,
  and `Weights(601, 1)` there is `[(3, 65535)]`.

The commit paid no transaction fee.

### Epoch 9: a second, separate operation

The epoch-9 window opened at confirmed block 8334771 and the coordinator
prepared operation `…e5e42a5a` from a preflight at 8334773 before the drills
were armed: nonce 1, era 8334773 to 8334837, round 33520119, predicted reveal
8335119. It was included at block 8334777 (index 6, success) and read back at
8334778. One submission per epoch so far, each with its own operation ID.

### Drill: lost acknowledgement (duplicate delivery), 11:26

`ack_failure` was armed at confirmed block 8334773. The next new delivery,
block 8334774, was committed and then answered 503. The provider retried the
same delivery ID with the same body; the journal deduplicated it
(`duplicate_deliveries` 1) and answered 204. The block was applied once, and
the retry drove no signing round. Three simultaneous `/signing/kick` calls a
minute later found no work and took no claim (`rounds` 0 each).

### Drill: stale holder and eviction after submit (epoch 10), 12:38

`stall_before_mark` and `evict_after_send` were armed before the epoch-10
window. This run used the original eviction drill, which called
`state.abort()` on the ChainJournal; the stale-holder half was voided by the
outage that call caused (below), so only epoch 11 shows fencing of a stale
holder.

| Time | Event |
| --- | --- |
| 12:38:06 to :15 | Preflight at confirmed block 8335133 created the epoch-10 operation; the registration step signed attempt 1 (nonce 2, era 8335133 to 8335197, round 33521559). Its submission step went to holder A with `stall_before_mark`: A's lease was cut to 2 s (`/sign-stall`) and A slept 15 s without marking |
| 12:38:19 | The next delivery's holder B took the claim (new generation; A's pending step dropped), got the same attempt's submission step with `evict_after_send`, marked it (`sends` 1), sent it, and called `state.abort()` on the ChainJournal without reporting |
| 12:38:21 to about 13:00 | **Outage.** The aborted instance stopped answering: every ChainJournal request (deliveries, ticks, status, A's late mark) hung until cancelled, 5 s for deliveries and 25 s for status. The cron watchdog could not help, since it goes through the same instance. The provider retried block 8335135 with backoff (1 s, 5 s, 30 s, 5 min; `endpoint timeout`); its next steps are 30 min and then a pause of the subscription, so about 15 more minutes of wedge would have needed a manual resume. A's mark never reached a working instance, so A never sent, and the stale-holder drill proved nothing this epoch |
| 12:53:46 | Redeploy: the instance stayed unresponsive while status polling continued |
| 12:59:28 | Status polling stopped; a second redeploy. `/status` answered at 13:00:20, in watchdog fallback (`webhook_silent`) |
| about 13:00 | The first signing round of the new instance made B's attempt `uncertain` (`abandoned_after_mark`). The nonce check at confirmed block 8335227 found nonce 3; the search found block 8335137, whose extrinsic 6 is the journaled bytes (`0x3098adf1…cbb977`); `ExtrinsicSuccess`; read-back `ok` at 8335227 |

No byte was sent twice (`sends` 1, `sign_submissions` 3 for three epochs) and
the chain holds one commit for epoch 10. Read independently from
`test.chain.opentensor.ai`: block 8335137 (`0x7399f594…f4eb2d59`) extrinsic 6
is the coordinator's commit with nonce 2, the finalized
`TimelockedWeightCommits(601, 10)` holds that one coordinator entry, and the
hotkey's nonce is 3.

The recovery evidence here comes from the abort variant: the instance that
recovered the attempt started only after two redeploys of 40924a90, a build
that still contained `state.abort()`. The eviction drill no longer calls it
(c196bfa9): called from the wasm fetch handler it is not a usable eviction in
workers-rs 0.8.7, and an operator redeploy evicts the instance instead. A
redeploy under steady request load did not restart the wedged instance; one
made with little traffic did. The current drill (stop after sending, then an
operator redeploy) ran live in the epoch-13 drill below.

### Drill: stale holder and uncertain response (epoch 11), 13:50

`stall_before_mark` and `uncertain_after_send` were armed at 13:03.

| Time | Event |
| --- | --- |
| about 13:50:15 | Preflight at confirmed block 8335493 created the epoch-11 operation and signed attempt 1 (nonce 3, era 8335493 to 8335557, round 33522999). Holder A got its submission step with `stall_before_mark`, had its lease cut to 2 s and slept 15 s |
| about 13:50:21 | Holder B took the claim (A's pending step dropped), got the same attempt with `uncertain_after_send`, marked it, sent it once, and reported `drill_response_discarded:accepted` (the node had accepted it): attempt `uncertain` |
| about 13:50:30 | A woke: its mark was refused (`mark_fenced`, `sign_fenced_marks` 1) and its late outcome was fenced (`sign_fenced_results` 1). A sent nothing |
| 13:50:31 | Three simultaneous `/signing/kick` calls took no claim (`rounds` 0 each): no confirmed block was new to the attempt at that moment, so this did not exercise claim contention live (retried in the epoch-13 drill below) |
| about 13:51 | The nonce check at confirmed block 8335498 found nonce 4; the search found block 8335497, whose extrinsic 7 is the journaled bytes (`0x7fefe8f2…b8ad7518`); `ExtrinsicSuccess`; read-back `ok` at 8335498 |

Read independently from `test.chain.opentensor.ai`: block 8335497
(`0x3a5d9851…56d414f2`) extrinsic 7 is the coordinator's commit with nonce 3.

### All commits

| Epoch | Nonce | Signed at | Included (index) | Reveal: predicted / observed | Drills | Sends |
| --- | --- | --- | --- | --- | --- | --- |
| 8 | 0 | 8334603 | 8334607 (6) | 8334759 / 8334764, weights match | none | 1 |
| 9 | 1 | 8334773 | 8334777 (6) | 8335119 / 8335124, weights match | none (`ack_failure` at 8334774) | 1 |
| 10 | 2 | 8335133 | 8335137 (6) | 8335479 / 8335484, weights match | stale holder, eviction after submit | 1 |
| 11 | 3 | 8335493 | 8335497 (7) | 8335839 / 8335844, weights match | stale holder, uncertain response | 1 |
| 12 | 4 | 8335853 | 8335862 (6) | 8336199 / 8336204, weights match | none (first epoch on the fixed code) | 1 |
| 13 | 5 | 8336213, then 8336278 | 8336282 (6), attempt 2 | 8336559 / 8336564, weights match | never sent (attempt 1 expired), eviction after submit (attempt 2), claim contention | 1 per attempt; attempt 1's bytes never sent |
| 14 | 6 | 8336573 | 8336577 (6) | 8336919 / pending at 17:27 | eviction after submit with an instance restart (trap) | 1 |

Rows 12 to 14 are from the afternoon run on the fixed code (below). Each
epoch has its own operation ID and exactly one inclusion: no resubmission and
no duplicate commit. Through epoch 14, `sign_attempts_signed` is 8 and
`sign_submissions` (marks) 8 for seven epochs: epoch 13 signed two attempts
with nonce 5, and its first was marked but, by the drill, never sent. The
other epochs were each included on their first send; only epoch 13
exercised the era-expiry path. Every observed reveal came five blocks after the predicted
block, when the drand pulse for the round (targeted three blocks past the
reveal block) reached the chain. In the morning `build-host` made 14 public
RPC calls for the independent checks; the afternoon checks are counted with
each drill.

### Signing cost

A `wrangler tail` from 11:19:40 to 11:29:30 (stored privately; it includes
request headers) covered the reveal search, the epoch-9 commit and one drill:

| Entry point | Count | CPU p50 / max (ms) | Wall p50 / max (ms) |
| --- | --- | --- | --- |
| DO `/sign-step` | 54 | 2 / 57 | 27 / 527 |
| DO `/sign-result` | 12 | 1 / 45 | 26 / 462 |
| DO `/sign-mark` | 1 | 1 / 1 | 26 / 26 |
| DO `/ingest` | 45 | 2 / 16 | 37 / 1008 |
| `/hook` | 46 | 1 / 2 | 106 / 1125 |
| cron | 9 | 30 / 59 | 936 / 2573 |

The maximum `/sign-step` includes re-parsing the stored metadata after a
restart; the maximum `/sign-result` includes timelock encryption and the
signature. A commit epoch costs about 10 to 15 RPC calls from the Worker
(preflight, registration, submission, one or two nonce checks, three to five
probes, the block, its events, the read-back, and three to five reveal reads),
inside the shared 12-a-minute budget; the per-minute count stayed at 5 or
less.

## M3 fixes, measured on 2026-10-30 (afternoon)

Deploys, by commit: 80b3404d (14:56: per-block runtime resolution,
durable failure accounting, the RPC sub-budget, the liveness record, schema
v5) and fd07442c (15:03:21: the durable reveal search, claim
release at a round's step limit, `chain_journal_starts`, schema v6), then
7663b3be (15:14:20: the instance view on `/status`). The journal
migrated in place twice with signing active: `/status` showed schema 5 at
14:57:08 (the head runtime's block seeded at 8334588 from the stored
metadata) and schema 6 at 15:04:28. The first liveness record appeared with
the first tick of the 80b3404d deploy; ticks answered in 100 to 200 ms (`ok`).

### Epoch 11 reveal and epoch 12 commit

| Time | Event |
| --- | --- |
| 15:00:15 | The epoch-11 reveal check (due at 8335841) ran at confirmed block 8335844: `gone, search 8335838..8335844`, then probes at 8335841 and 8335842 (both `queued`) |
| 15:00:30 to 15:03 | The search restarted from the queue check on every delivery round: a delivery allows three steps, the search needed more, and its bounds were not stored, so each round repeated the check and two probes (log rows 53 to 64). Signing hit its 8-call share (`sign_budget_deferrals` 3, then 6); the heartbeat kept its calls. This was a defect of the earlier search as well, hidden while cron ticks (eight steps) finished it. Fixed in fd07442c |
| about 15:02:40 | Epoch-12 preflight at confirmed block 8335853: operation created, registration read, then the new signing-runtime step read version 486/1 at 8335853 (the head runtime's block moved to 8335853) and signed attempt 1 (nonce 4, era 8335853 to 8335917, round 33524439). That was the round's third and last step; under 80b3404d the round still held the claim until its lease expired |
| about 15:03:10 | The next holder marked and submitted it; accepted |
| 15:03:21 | Redeploy (fd07442c) while the attempt was `submitted` (not `sending`), so nothing was uncertain |
| about 15:04 | The epoch-11 search, now stored, continued at 8335859: probes 8335848 `gone`, 8335843 `queued`, 8335845 `gone`, 8335844 `gone`; the events at 8335844 hold the coordinator's `TimelockedWeightsRevealed`: `revealed`, `weights_match_row` (five blocks after the predicted 8335839, like every reveal so far) |
| about 15:05 | Epoch 12: nonce consumed at confirmed block 8335864; the search found block 8335862, whose extrinsic 6 is the journaled bytes; `ExtrinsicSuccess`; read-back `ok` at 8335864 |

Read independently from `test.chain.opentensor.ai` (2 calls): block 8335862
(`0x74346391…`) holds 7 extrinsics, and the blake2-256 of extrinsic 6 equals
the journal's extrinsic hash `0xeb7d927a3079d703…`.

### Drill: a trap in the ChainJournal under load, 15:05

`trap` was armed at 15:05:54, with the normal load running: a delivery every
block (each with a signing round), the cron each minute, and a status poll
every 3 s (a private operator script and log).

| Time | Event |
| --- | --- |
| 15:05:53 | Before: `chain_journal_starts` 5, cursor 8335871, claim generation 1508, liveness `ok` |
| about 15:05:55 | The next delivery's `/sign-step` took the drill (logged as `drill_trap`, row 83, in its own committed transaction) and panicked outside any transaction |
| 15:05:57 | `/status` answered in 351 ms with `chain_journal_starts` 6: workers-rs had marked the Wasm instance broken and rebuilt it, and the ChainJournal object, on the next request |
| 15:05:57 to 15:09:54 | Deliveries kept advancing the cursor (8335872 to 8335891), signing rounds kept taking and releasing the claim (generation 1508 to 1531), status answered every 3 s (at most 609 ms), and every cron tick answered (`ok`) |

So a Rust panic in the DO path costs one failed request and a rebuild of the
instance within about 2 s; nothing wedged and no operator action was needed.
This qualifies the panic path, not `state.abort()`, which wedged the instance
for about 21 minutes earlier in the day. A `wrangler tail` alongside showed
the Worker's own invocations (`/hook` 21, `/status` 83, cron 4, all `ok`) but
no ChainJournal events, so the instance rebuild is evidenced by the counter
and the uninterrupted cursor, not by a log line.

`chain_journal_starts` also rose without a drill: 5 constructions between the
15:03:21 deploy and 15:05:53, one each at about 15:10:30 and 15:12, and, with
the instance view of 7663b3be (polled every 10 s), at
15:26:08, 15:27:08, 15:29:08 and 15:35:24. Those four were not Wasm resets:
the module's memory carried over (4.1, 5.5, 6.8 and 7.3 MB, never back to a
fresh 1.4 MB), so only the ChainJournal object was constructed again in a
running isolate. A 10-minute `wrangler tail` from 15:29
(raw output private) captured 544 events, all
`ok`, with no exception and no console line in the 20 s before the 15:35:24
construction. So these look like platform restarts of the Durable Object,
not traps. Their rate tracks how often the instance gets requests (counted
from `chain_journal_starts` in the poll logs):

| Window | Requests besides deliveries (about every 12 s) and the cron | Constructions |
| --- | --- | --- |
| 15:19:30 to 15:44:00 | status polls every 10 s and 15 s | 4 in 24.5 min |
| 15:44:30 to 16:02:30 | a status poll every 15 s | 34 in 18 min |
| 16:03:30 to 16:13:50 | status polls every 10 s and 15 s | 2 in 10 min |
| 16:14:10 to 16:28:40 | a status poll every 3 s | 0 in 14.5 min |

That fits an instance evicted after roughly ten seconds without a request,
which the gaps between deliveries alone often exceed; it is an inference from
these counts, not a documented limit. Each construction was harmless,
because it re-reads durable state, and costs one re-parse of the stored
metadata at the next signing step. Memory was 5.9 MB by 16:30 (a fresh
isolate had replaced the old one), so it does not accumulate without bound.
Since the arena migration the linear memory has the model's ceiling, growable
in the current `Observe` build (`instance.arena.memory_bytes` is what the
isolate holds); the arena counters on `/status` (`instance.arena`:
pages in use, overdrafts, quarantined and abandoned scopes, per-route peaks)
are what to watch. A deploy took 1 to 5
minutes to reach the running instance (15:03:21 to 15:04:28; 15:14:20 to
15:19:28).

### Drill: never sent, era expiry, eviction after submit, claim contention (epoch 13), 16:14

`never_send` and `evict_after_send` were armed at 15:07 (a private operator
script and log). The submission drills are taken one per submission
step in a fixed order, so attempt 1 took `never_send` and attempt 2
`evict_after_send`.

| Time | Event |
| --- | --- |
| 16:14:20 | Preflight at confirmed block 8336213 created the epoch-13 operation and signed attempt 1 (nonce 5, era 8336213 to 8336277, round 33525879). Its submission step took `never_send`: the attempt was marked (`sends` 1 counts the mark) and the bytes were never sent; the Worker reported `drill_never_sent`, so the attempt became `uncertain` |
| 16:14:24 to 16:16:13 | 40 batches of three simultaneous `/signing/kick` calls, every 3 s. At 16:15:15, right after the watermark moved to 8336218, one kick took the claim and ran the nonce check at the new block (1 round, 506 ms) while the other two found the claim held and skipped (0 rounds in 106 and 121 ms; `sign_claim_held_skips` 13 to 15). Every other batch found no new block and took no work. This is the claim contention the earlier runs did not catch |
| 16:14 to 16:27 | Nonce checks at each new confirmed block found nonce 5 unconsumed, so no second attempt was signed while the first could still be included |
| 16:27:16 | At confirmed block 8336278, past the expiry 8336277 with the nonce still 5: attempt 1 `expired`, operation back to `preparing` |
| 16:27:19 | Preflight at 8336278 signed attempt 2 with the same nonce 5 (era 8336278 to 8336342, same round). Its submission step took `evict_after_send`: marked, sent, never reported |
| 16:27:19 to :29 | The operator redeploy (the same commit) finished in 10 s, but, as with the earlier deploys, the new version reached the running instance only minutes later |
| 16:27:45 | When the evicted holder's 25 s lease lapsed, the next holder found the mark without an outcome: attempt 2 `uncertain` (`abandoned_after_mark`); nothing was resent |
| about 16:28:30 | Nonce consumed at confirmed block 8336284; the search found block 8336282, whose extrinsic 6 is attempt 2's bytes; `ExtrinsicSuccess`; read-back `ok` at 8336284 |

Read independently from `test.chain.opentensor.ai` (11 calls; file
`evidence/epoch13-onchain.json`): the hotkey's nonce was 5 at 8336277 (the
expiry block) and at 8336281, and 6 at 8336282, so it moved exactly once;
block 8336282 (`0x187ce87f…`) holds 7 extrinsics, the blake2-256 of
extrinsic 6 equals attempt 2's journaled hash and attempt 1's hash is not in
the block; and the finalized `TimelockedWeightCommits(601, 13)` holds exactly
one coordinator entry. One inclusion for two attempts with the same nonce.

So the era-expiry path, the only one that signs a second attempt with an
already-used nonce, ran live and produced exactly one inclusion. The eviction
half shows recovery after a lapsed lease, not after an instance restart: the
redeploy did not reach the instance in time. The epoch-14 run below restarts
the instance between the send and the recovery.

### Drill: eviction after submit with an instance restart (epoch 14), 17:26

`evict_after_send` was armed at 16:30 (a private operator script and log);
the driver armed `trap` as soon as the attempt was
`sending`, so the instance would restart between the send and any outcome.

| Time | Event |
| --- | --- |
| 17:26:25 | Preflight at confirmed block 8336573 signed attempt 1 (nonce 6, era 8336573 to 8336637, round 33527319). Its submission step took `evict_after_send`: marked, sent, never reported; the dead holder's claim stayed held. `trap` armed |
| 17:26:32 | The next `/sign-step` took the trap (`drill_trap`, row 117) and panicked; `/status` showed `chain_journal_starts` 151 to 152 and an instance age of 0, with the attempt still `sending` and no outcome recorded |
| 17:26:45 | After the lease lapsed, the new instance's first holder found the mark without an outcome: `uncertain` (`abandoned_after_mark`); nothing was resent |
| 17:27:15 to :18 | Nonce consumed at confirmed block 8336578; the search found block 8336577, whose extrinsic 6 is the journaled bytes; `ExtrinsicSuccess`; read-back `ok` at 8336578 |

Read independently from `test.chain.opentensor.ai` (5 calls; file
`evidence/epoch14-onchain.json`): the hotkey's nonce was 6 at 8336576 and 7 at
8336577, and the blake2-256 of extrinsic 6 of block 8336577 (`0xea139f90…`)
equals the journal's hash. With epoch 13, this covers the drill as designed:
an attempt sent and never reported, the instance replaced before any
outcome, and recovery from durable state alone, with one send and one
inclusion.

After these runs (17:27): `sign_attempts_signed` 8 and `sign_submissions` 8
for seven epochs (8 to 14), `sign_recovered_uncertain` 3,
`sign_claim_held_skips` 18, `sign_budget_deferrals` 9, and no step failures
or RPC failures since the fixed code was deployed.

## Monthly platform cost (M1 to M3)

The coordinator's requests, CPU, Durable Object use, SQLite rows and storage
stay within the plan's included amounts at the measured rates (about 0.3M
Worker requests and 0.6M Durable Object requests a month; at most 0.32M GB-s
of Durable Object duration; about 0.8 MB of metadata per runtime and a few
rows per epoch), and both buckets were empty. Pruning the journal (README,
"Not yet implemented") keeps storage flat. Account-level charges and
estimates are kept in the private operator inventory.

## Hidden evaluation Worker (M4)

`slop-ninja-eval-testnet` is the same crate deployed with `--env eval-testnet`
and `EVAL_ROLE = "eval"` (README, "Hidden evaluation", shortcut T13). It has its
own chain journal, webhook subscription and cron watchdog, plus the cohort
authority and the scorer Container.

| Resource | Kind | Notes |
| --- | --- | --- |
| `slop-ninja-eval-testnet` | Worker on workers.dev | URL `$EVAL_URL`, recorded in the private inventory; `limits.cpu_ms = 120000`; observability off |
| cron `* * * * *` | Workers cron trigger | watchdog plus one `state_queryStorageAt` epoch read per tick; kicks both cohort instances |
| `slop-ninja-eval-testnet_ChainJournal`, `_EvalCohort`, `_AScorer` | Durable Object namespaces (SQLite) | bindings `CHAIN_JOURNAL_TESTNET`, `EVAL_COHORT_TESTNET` (instances `main` and `load`), `A_SCORER_TESTNET` (scoring lanes `a-scorer/testnet/{main,load}/lane-<k>`, the probe lane `a-scorer/testnet/probe`) |
| `slop-ninja-eval-testnet_EvalLane`, `_GenLimiter`, `_SpendLedger` | Durable Object namespaces (SQLite, migration `v2`) | bindings `EVAL_LANE_TESTNET` (generation, summary, bundle and probe lanes), `GEN_LIMITER_TESTNET` (`gen-limiter/testnet`, token buckets in memory), `SPEND_LEDGER_TESTNET` (`spend-ledger/testnet`) |
| `eval-lanes-queue-testnet` | Queue | producer `EVAL_LANES_TESTNET` and consumer on the eval Worker (batch 100, timeout 1 s, 10 retries, concurrency unset); carries content-free lane kicks only |
| `a-scorer-testnet` | Container application | standard-4; `max_instances` 64 at rest, raised in steps (128, 256, 375) only before wide runs and lowered after, equal to the var `MAX_SCORE_LANES`; 0 instances at rest; one instance per scoring lane, started with Internet off and stopped by its lane (120 s idle stop as a backstop) |
| Workers AI | binding `AI` | generators listed in the plan; one shared limiter |
| `eval-subscription-testnet` | rtao-rpc testnet webhook subscription | stream `finalized`, key `pilot-v1`, delivering to `/hook` |

Secrets (stdin only): `EVAL_PRIVATE_SEED`, `EVAL_PACKAGE_SIGNING_KEY`,
`EVAL_RECEIPT_SIGNING_KEY` (32-byte hex; Ed25519 application keys, never chain
keys), `COORDINATOR_STATUS_TOKEN`, `COORDINATOR_OPERATOR_TOKEN`,
`RTAO_WEBHOOK_SIGNING_SECRET`, `RTAO_WEBHOOK_SUBSCRIPTION_ID`. The two signing
secrets are the T12 package and receipt seeds, piped from their mode-600 seed
files; the vars `PACKAGE_PUBLIC_KEY` and `RECEIPT_PUBLIC_KEY` pin the public
roots ([`testnet-trust.json`](testnet-trust.json)), and the Worker refuses to
sign when a secret does not derive its root. `GET /eval/status` reports the
public keys, or `signing_keys_error`.

### Prepare and deploy

```sh
# Private pool and the R0 artifact (content-addressed parts) in the eval bucket.
slop_ninja_a_testnet_eval_manifest pool --repo . --pool-id testnet-human-pool-v1 --out-dir <dir>
slop_ninja_a_served_profile pack --model-dir <release model dir> --out <dir> --part-mib 64
wrangler r2 object put eval-bucket-testnet/pools/testnet-human-pool-v1/<file> --file <file> --remote
wrangler r2 object put eval-bucket-testnet/artifacts/blobs/<sha256> --file <part> --remote
wrangler r2 object put eval-bucket-testnet/artifacts/packages/<package>.json --file artifact-files.json --remote

# Scorer binary (pinned rust:1.99.1-slim-bookworm, checkout mounted read-only),
# copied with the ORT library into the ignored build context.
#   inference/a-container-serve/container/build/{ort/libonnxruntime.so.1.31.1,bin/a-scorer}
wrangler deploy --env eval-testnet --tag testnet --message "slop-ninja eval testnet <sha>"
```

The deploy runs `worker-build` and builds and pushes the scorer image, so run
it in a bounded build job as above. Register the
subscription as for the coordinator, with name `eval-subscription-testnet` and URL
`.../hook` on the eval Worker, install its two secrets, then `verify`.

### Operate

```sh
# EVAL_URL: the eval Worker URL, from the private inventory.
# status (content-free): phases, counts, generation usage and cost, jobs, digests
printf 'Authorization: Bearer %s\n' "$(cat $S/eval_status_token)" |
  curl -sS --header @- $EVAL_URL/eval/status
# queue a plan (operator token in a stdin header, the plan as a file body)
printf 'Authorization: Bearer %s\n' "$(cat $S/eval_operator_token)" |
  curl -sS --header @- -H 'Content-Type: application/json' --data-binary @plan.json \
    $EVAL_URL/eval/plan
# pre-flight a generator or the scorer path on fixed synthetic text: /eval/probe, /eval/scorer-probe
# resume after an outage fault: /eval/clear {"label": "..."}
#   (a terminal fault answers {"cleared": false, "refused": "abandon_required"})
# end a terminally faulted cohort without a target: /eval/abandon {"label": "...", "reason": "<code>"}
# the load-rehearsal instance: GET /eval/load/status; POST /eval/load/{plan,clear,abandon,hold}
#   (a load plan is slop_ninja_eval_load_plan_v1; {"hold": true} makes every claim answer Paused)
# load plans and gates: examples/slop_ninja_eval_ops.rs (load-plan; gate --for deploy|load on both
#   status documents before any deploy or load run; summary, watch-line, copy-ties)
# lane objects' own state: POST /eval/lanes/state {"names": [...]} (at most 64)
# platform probes: POST /eval/probe/connections {"kind": "ai"|"do"|"r2", "n", "model", "max_tokens", "ms"}
#                  POST /eval/probe/rate {"models", "rpm_per_model", "seconds", "lanes_per_model"}; /eval/probe/rate-state
```

`/eval/status` (and `/eval/load/status`) shows, per cohort, the lane summary
(`lanes`: counts by kind and status, kicks and re-kicks, starts and start
failures, lost and reconciled attempts, fenced commits, cold-path, cold-path
CPU and chunk-time percentiles, host CPU models, generation call times and
throttles, the timeline and spend against the scoring budget and the ledger),
the job counts by status (with each job's isolated crashes, artifact failures
and any recorded artifact failure), hand-back and attribution events
(`given_back`, `iso_yield`, `iso_yield_failure`, `iso_resumed`,
`artifact_failure`) in all and per job, the last dispatch (kicks queued, sent
directly or dropped by a load fault hook), the limiter's buckets and the
ledger's standing.

**Spend guards.** Cloudflare offers budget alerts but no hard spending cap, so:

1. `max_instances` is the platform's per-application ceiling. Raise it in
   steps only before a wide run, keep `MAX_SCORE_LANES` equal, and lower it
   afterwards (it rests at 64). A change rolls the application out again:
   wait until `wrangler containers info <id>` shows the new version settled
   (no `scheduling` backlog) before kicking wide work. Starts beyond the
   application's prepared (`healthy`) instances at the moment of the request
   stayed unplaced until their lane destroyed and retried them after
   `SCORER_START_WAIT_MS` (2026-10-31), so the first wide run after a step
   pays one start bound for most of its lanes.
2. Every plan declares `fanout.score.scoring_budget_microusd`. A plan whose
   own upper bound (J x n x `doc_ub` + lanes x `cold_ub`) exceeds it is
   refused; each sweep checks measured spend, a bound for lanes presumed dead
   and the remaining work's bound, and faults the cohort (`scoring_budget`,
   terminal) before it can be exceeded.
3. The `SpendLedger` keeps the month: a plan's generation and scoring budgets
   are reserved against `MONTHLY_CAP_MICROUSD` (a deployment value passed at
   deploy time, not tracked; a testnet deployment without it refuses every
   reservation, mainnet has none) before it queues
   (refused with `monthly_cap` otherwise), and a finished cohort settles its
   measured spend, once. Spend is keyed by the billing cycle starting on
   `BILLING_CYCLE_DAY`; set it to the account's real cycle day. Every open
   reservation holds headroom in every cycle, and a label already queued never
   touches the ledger again (a retried `/plan` answers `already`).
4. Lanes stop their containers when their job is done and when the cohort
   pauses; the inactivity timeout and the 120 s idle alarm are backstops.
5. Budget alerts are a dashboard action (they only email); their thresholds
   are in the private operator inventory.

To verify a package, fetch the cohort's objects from the receipts bucket
(`wrangler r2 object get receipts-bucket-testnet/cohorts/<label>/<name> --file <absolute path> --remote`,
every key in `chunk-map.json`), take the keys from `testnet-trust.json`, refuse
if the cohort's `keys.json` differs, and run `examples/eval_verify.rs` with the
pinned keys. Then run the native validator's `package::verify` on the written
T12 bundle under a configured validator's `[package.trusted_keys]` (all three
roles).

A `reward-v1` cohort (plan v3) adds `published-contents.bin` and
`fingerprint-index.bin` at the seal and, after its closing boundary,
`closure.signed`, `published-contents-after.bin`, `fingerprint-index-after.bin`
and `closure-fingerprints.json`; `target.signed` exists only when the cohort
produced a target. Verify it with `examples/reward_v1_verify.rs`:

```sh
reward_v1_verify cohort <dir> --trust testnet-trust.json \
  --snapshot-owner <owner hotkey> --snapshot <uid>:<hotkey>:<reg> ... --snapshot-at <block>:<hash> \
  --authorization-seed <mode-600 seed file> --issued-epoch <closing epoch>
reward_v1_verify launch --trust testnet-trust.json --authorization-seed <file> --issued-epoch <e> --out launch.json
```

It recomputes the outcome, checks the closure and writes
`reward-v1-bundle.json` (with the receipt index and chunks, for the native
validator's `[package] path`) and `reward-v1-bundle-coordinator.json` (for
`POST /signing/target` on the coordinator). With `--snapshot-at`, the row it
derives carries that block's snapshot binding, so its digest must equal the
row digest the coordinator (`/status` ops) or the validator (`row_derived`)
reported for the same block. The authorization seed is the T12 authorization
key; it is read from its file and never printed.

### Teardown

```sh
hook_api --request DELETE "$RTAO_WEBHOOK_BASE/v1/subscriptions/eval-subscription-testnet"
wrangler delete --env eval-testnet            # Worker, cron, secrets, DO namespaces (lanes, limiter, ledger)
wrangler queues delete eval-lanes-queue-testnet
wrangler containers list; wrangler containers delete <id>
wrangler containers images list; wrangler containers images delete a-scorer-testnet:<tag>
# then delete the eval and receipts bucket objects (pools/, artifacts/, cohorts/),
# including the load cohorts' cohorts/load-*/ objects in both buckets
```

`wrangler delete` removes the cohort authority's Durable Object, including its
used set. While the pool may still be drawn by any later authority (T13
removal, a recreated Worker), keep `pools/<pool>/used/`: it is the durable
never-reuse ledger every draw reads.

### Rehearsal 1: K = 4, n = 200 (2026-10-30)

Plan `testnet-m4-rehearsal-1-k4-n200` (cohort 1): 40/40/20, K = 4, admission
and transition 1 tempo each, max 3 attempts, 5 generators (llama-3.1-8b-instruct-fp8-fast,
llama-3.2-3b-instruct, granite-4.0-h-micro, mistral-small-3.1-24b-instruct,
mistral-7b-instruct-v0.1), roster R_0 (release candidate seed 17,
owner UID 0) plus a duplicate entrant (the same package, owned by miner1 at
UID 3), `testnet-reward-v0`. Generation and scoring ran on a single deployed
version.

| Step | When (UTC) | Duration | Notes |
| --- | --- | --- | --- |
| queued | 10:55 | | before any epoch was observed |
| open (admission) | 11:25:03 | 29.4 min wait | epoch index 8 -> 9 at block 8334759, observed at confirmed block 8334763 |
| generating | 12:37:03 | 72.0 min | index 9 -> 10, one tempo |
| draw | 12:37:06 | 2 s | 80 humans as 71 whole source groups, domain quotas, no shortfall, 490 left |
| generation | to 12:41:25 | 262 s | 120 documents, 123 calls, 3 rejected (length), 23,587 + 23,128 tokens |
| seal | 12:41:27 | 1.7 s | 200 tasks, 4 private shards and a text-free index; manifest signed |
| scoring | to 12:46:09 | 282 s | 2 jobs x 200 tasks on 2 slots; see below |
| packaging | 12:46:10 | 1.0 s | index, package, target, chunk map and components stored |
| transition | to 16:12:04 | 3 h 26 min | index 13 = opened + K at block 8336199, observed at confirmed block 8336202 |
| closed | 16:12:04 | | target `c5e09990...` active; private texts deleted from the Durable Object; rehearsal 2 opened at the same boundary |

- **Generation by model** (calls / tokens in / out / mean ms): granite 25 /
  5,533 / 3,817 / 4,911 (3 rejected); llama-3.1-8b 23 / 4,925 / 4,303 / 1,130;
  llama-3.2-3b 27 / 4,988 / 4,970 / 1,094; mistral-7b 17 / 3,069 / 4,192 /
  14,586; mistral-small 31 / 5,072 / 5,846 / 3,479. Mixed share averaged
  0.42 (0.013 to 0.878); documents averaged 224 words.
- **Scoring.** Each slot cold-started and streamed the 601 MB artifact from R2
  in 29-32 s. Scoring ran at about 3.4-4.2 documents/s per slot (chunks of 32
  in 7-9 s), 366 container CPU-s in all. The first chunk of each job was lost:
  the step that held both claims ended without committing (cause not
  captured: the tail sampled no event for that invocation, and a
  kick-scheduled alarm was queued behind it). R2 holds no
  `job-00{0,1}/chunk-0000-attempt-1.bin`, so the invocation stopped after
  claiming and before its first receipts put. After the 240 s lease both chunks
  were claimed again, the lost attempt was recorded as 32
  infrastructure-error receipts per job, and attempt 2 succeeded on freshly
  restarted containers. Deployed before rehearsal 2:
  kicks no longer schedule a step while one runs, the lease is 150 s (the step
  budget stayed 60 s), commit failures surface on `/eval/status`, scorer slots
  are started and loaded before any chunk is claimed, and slots stop when
  scoring ends.
- **Receipts.** 464 receipts (400 predictions, 64 infrastructure errors) in 14
  chunk objects (71,920 bytes). `eval_verify` fetched every object from the
  receipts bucket and verified the signatures, the manifest, package, target
  and index bindings, every chunk digest and every receipt against its index
  entry; it derived the row `[(0, 65535)]` and wrote a T12 bundle. The native
  validator's own `package::verify`, given the same keys (and a placeholder
  authorization key), accepted that bundle and refused it with one
  receipt byte changed (`receipt_object_mismatch`).
- **Scores** (plumbing only, not quality evidence). R_0 and the duplicate are
  identical: v0 utility 0.8730, Brier 0.127 (human_only 0.057, mixed 0.251,
  model_only 0.019), share MAE 0.10 (mixed 0.17), human FPR at 0.5 of 5 %, TPR
  70 %, every human_only document attributed `other_human`. Paired gain 0 and
  prediction RMS 0: one near-duplicate cluster, so the copy earns nothing and
  the row burns to UID 0.
- **No text in logs.** 600 40-character samples of the 200 batch texts were
  searched in two `wrangler tail` captures covering generation, sealing,
  scoring and packaging (879 KB, 335 events), every `/eval/status` snapshot, the
  components, chunk map, T12 bundle and batch index: 0 hits (the same check on
  a batch shard itself: 234 hits). The Worker logs nothing; one generic runtime
  message was seen during development, from a malformed operator request.
- **Usage.** Containers about 900 instance-s (two sessions per slot,
  including the reload and idle tails) and 366 CPU-s; Durable Object duration
  21 steps, 352 s at 128 MB.

### Rehearsal 2: K = 4, n = 1,000 (2026-10-30)

Plan `testnet-m4-rehearsal-2-k4-n1000` (cohort 2): the same settings with
n = 1,000, queued during rehearsal 1. It ran on the deploy made before rehearsal 2 (scorer slots
prepared before any claim, no overlapping steps, 150 s lease, slots stopped at
packaging).

| Step | When (UTC) | Duration | Notes |
| --- | --- | --- | --- |
| open (admission) | 16:12:04 | | at rehearsal 1's close: index 13, block 8336199 |
| generating | 17:25:03 | 73.0 min | index 14 at block 8336559, observed at 8336567 |
| draw | 17:25:05 | 1.7 s | 400 humans as 356 whole source groups, no shortfall, 90 left |
| generation | to 17:53:08 | 28.1 min | 600 documents, 613 calls, 13 rejected for length (12 granite, 1 mistral-small), 119,833 + 113,730 tokens |
| seal | 17:53:14 | 5.3 s | 1,000 tasks, 16 private shards |
| scoring | to 18:00:24 | 430 s | 2 jobs x 1,000 tasks; slots loaded in 25.6 s and 41.3 s before any claim |
| packaging | 18:00:33 | 8.9 s | |
| transition | to 21:00:03 | 3 h 0 min | |
| closed | 21:00:03 | | index 17 = opened + K at block 8337639, observed at 8337642; target `ec067f0d...` active, its predecessor rehearsal 1's target `c5e09990...` |

- **Generation by model** (calls / accepted / tokens in / out / mean ms):
  granite 131 / 119 / 23,644 / 23,196 / 5,459; llama-3.1-8b 113 / 113 /
  23,117 / 21,109 / 1,147; llama-3.2-3b 126 / 126 / 26,579 / 22,007 / 1,054;
  mistral-7b 117 / 117 / 25,108 / 25,673 / 12,972; mistral-small 126 / 125 /
  21,385 / 21,745 / 3,304.
  Mixed share averaged 0.42; documents averaged 222 words. Waves of 12 calls in
  groups of 6 are paced by the slowest call, so mistral-7b (13 s) sets most of
  the 28 minutes.
- **Scoring.** 66 scorer requests, about 7-10 s per 32-document chunk per slot,
  1,856 container CPU-s. One chunk per job was lost once: chunk 18 of both jobs,
  mid-run, after 18 chunks per job had committed on slots that were already
  prepared, so the cold path does not explain it. An alarm invocation holding
  both claims ended without committing, and the tail again recorded no event
  for it. R2 holds no `job-00{0,1}/chunk-0018-attempt-1.bin`, so it stopped
  after claiming and before its first receipts put. The claims expired after
  150 s, the lost attempt became 32 infrastructure-error receipts per job
  (`chunk-0018-attempt-2.bin`), and attempt 2 succeeded. Metered instance time was 431 s
  and 432 s per slot, with no idle tail (one start each, stopped at packaging);
  the meters had not moved by the close three hours later, and Wrangler listed
  0 live instances.
- **Receipts.** 2,064 receipts (2,000 predictions, 64 infrastructure errors) in
  64 chunk objects (319,920 bytes). `eval_verify` and the native validator's
  `package::verify` both accepted the stored package under the eval Worker's
  own keys; the row is `[(0, 65535)]`.
- **Never reused.** The two cohorts' used sets share no row, text digest or
  source group, and every used group was drawn whole: 480 of the pool's 570
  rows are used.
- **Scores** (plumbing only). R_0 and the duplicate: v0 utility 0.8685, Brier
  0.132 (human_only 0.076, mixed 0.243, model_only 0.019), share MAE 0.11
  (mixed 0.17), FPR at 0.5 of 6 %, TPR 73 %; paired gain and SE 0 over 556
  source-group clusters, RMS 0, one cluster.
- **No text in logs.** 2,999 samples of the 1,000 texts: 0 hits in four tail
  captures (1.69 MB covering generation through packaging), the status
  snapshots, components, chunk map, T12 bundle, verifier report and batch
  index (the check on one shard itself: 252 hits).
- **Usage.** Containers 863 instance-s and 1,856 CPU-s; Durable Object
  duration 87 steps, 1,841 s at 128 MB.

### Review fixes and rehearsal 3: K = 3, n = 10 (2026-10-30)

The review fixes (README, "Hidden evaluation") were deployed from commit
`9051a52a` (21:49:40 UTC), which migrated the live
`EvalCohort` store from v2 to v3 in place (both closed cohorts intact,
`schema_version` 3). The Worker then refused to sign
(`signing_keys_error`) until the two signing secrets were replaced by the T12
package and receipt seeds (21:49:56-57); it has since reported the T12 public
keys. Later redeploys: `9cd8988f` (re-check of an accepted stop)
and `f8c7841c` (authority id on status), during admission and transition.
Nothing was deployed between the deploy before rehearsal 2 and the review
fixes:
rehearsal 2 ran with the 60 s step budget and with replies committed only
after every reply of a step returned. Per-chunk commits first ran here.

**Re-verification of rehearsals 1 and 2** from R2 with the keys pinned in
`testnet-trust.json`: under the T12 roots both are refused, by `eval_verify`
(manifest signature `UntrustedKey`) and by the native validator's
`package::verify` under validator2's own `trusted_keys.toml`
(`manifest_unverified: untrusted_key`). Under the pinned earlier eval keys
both verify (464 and 2,064 receipts). They show internal consistency only.

**Scorer stop.** A synthetic `/eval/scorer-probe` at 21:50:59 cold-started
slot 0 and loaded the artifact in 30.7 s. The instance stopped by itself 60-75 s
after its last use, before the watchdog's 60 s idle threshold met a cron tick
and before its own 120 s idle alarm (`sweep_stops` 0, `stop_failures` 0); the
alarm then closed the metered session. The platform also stops idle
instances, which explains the "freshly restarted containers" after rehearsal
1's lease gap. The sweep's stop and the failed-stop re-check were therefore
not exercised live.

Plan `testnet-m4-rehearsal-3-k3-n10` (cohort 3): 40/40/20, K = 3, admission
and transition 1 tempo each, max 3 attempts, the same 5 generators,
R_0 plus the duplicate entrant, chunks of 2 documents (5 per job) on
2 slots, tolerance 0.

| Step | When (UTC) | Notes |
| --- | --- | --- |
| queued | 21:50:48 | epoch 17; active tempos 0 (the v3 clock starts at the migration) |
| open (admission) | 22:12:04 | index 17 -> 18 at block 8337999, observed at 8338002; heartbeat fresh, so active tempo 1 = `opened_active` |
| generating | 23:24:03 | index 19 (active 2) at block 8338359, observed at 8338362 |
| draw | 23:24:04 | 4 humans as 4 whole groups, no shortfall; the draw read the R2 ledger (480 rows, 427 groups from rehearsals 1-2) and wrote its v2 entry first |
| generation | to 23:24:30 | 6 documents, 6 calls, no rejection, no outage, 1,118 + 1,052 tokens |
| seal | 23:24:31 | 10 tasks: counts 4/4/2 = plan, shortfall 0/0/0, bound in the batch index |
| scoring | to 23:25:17 | slots started and loaded 23:24:31-23:25:09 before any claim; then 10 requests in 8 s, each committed as its reply returned |
| packaging | 23:25:20 | `packaged_active` 2 |
| transition | to 01:49:05 | active tempo 3 at index 20 (00:37); index 21 at block 8339079, observed at 8339087, is active tempo 4 = opened + K |
| closed | 01:49:05 | target `0e7042d5...` active; private texts deleted; the next queued cohort opened at the same boundary (`opened_active` 4) |

No heartbeat gap occurred, so every increment was credited (0 uncredited) and
the close fell at the same index the raw difference gives; the silence rule
itself is covered by the native tests.

- **Attempts.** Every chunk ended with allocated attempt 1 and recorded attempt
  1: no lost claim, no not-dispatched return, no fault and no step error. 20
  receipts (all predictions) in 10 chunk objects (3,100 bytes).
- **Verification** from R2 under the pinned T12 roots: `keys.json` matched the
  pin; `eval_verify` verified the signatures, bindings, every chunk digest and
  every receipt, and derived the row `[(0, 65535)]`; the native validator's
  `package::verify` accepted the T12 bundle under validator2's own
  `trusted_keys.toml` (package, receipt and authorization roots) and refused it
  with one receipt byte changed (`receipt_object_mismatch`). Manifest
  `c60ac754...`, package `07043270...`, target `0e7042d5...`, whose predecessor
  is rehearsal 2's target `ec067f0d...`.
- **Ledger.** The eval bucket holds three entries for the pool: rehearsals 1
  and 2 (v1, 80 and 400 rows) and rehearsal 3 (v2, 4 rows, naming authority
  `194bf06c...`): 484 distinct rows and text digests and 431 distinct groups,
  equal to the Durable Object's 484 used rows. 86 humans remain.
- **No text in logs.** 30 samples of the 10 texts: 0 hits in a `wrangler tail`
  capture from 23:20:33 to 23:29:34 (454 KB), every status snapshot, the
  components, chunk map, T12 bundle, verifier reports, `keys.json` and the batch
  index (561 KB in 15 files); the same check on the shard itself: 30 hits.
- **Usage.** Containers about 95 instance-s and 15 CPU-s; the probe about 44
  instance-s.

### Drill 4: a pool shortfall, a refused clear and abandonment (2026-10-31)

Plan `testnet-m4-drill-4-shortfall-n4000` (cohort 4) asked for n = 4,000
(1,600 humans) from a pool with 86 left. It was queued behind rehearsal 3 and
opened at rehearsal 3's close (index 21, block 8339079, `opened_active` 4).

| Step | When (UTC) | Notes |
| --- | --- | --- |
| generating | 03:00:03 | index 22 (active 5), observed at block 8339442 |
| `draw_shortfall` | 03:00:05 | the draw read the ledger, took all 86 remaining humans, wrote its ledger entry, then found 86/86/800 against 1,600/1,600/800 (tolerance 0) and paused with no document created and no human marked used (still 484); its ledger entry was deleted (R2: key does not exist) |
| clear | 03:01 | refused: `{"cleared": false, "refused": "abandon_required"}` |
| abandon | 03:01:13 | a reason that is not a code was refused (422); `pool_shortfall_drill` was accepted once (a repeat answered `false`). Phase `abandoned` (the v3 phase check in Durable Object SQLite), no target; the active target stays rehearsal 3's `0e7042d5...`; no alarm, no step error, 0 container instances |

No generation, scoring or container time was used.

### Fan-out lanes: deploys and platform probes (2026-10-31)

Branch `subnet-testnet/fanout`. Deploys to `slop-ninja-eval-testnet`. Four
of the five came between cohorts; the `e0b39580` deploy did not: it went live at
04:23:48 while `load-s2-r0p40-n1000` was packaging (04:17:44 to 04:24:43),
against the design's rule that no deploy happens while a cohort generates,
seals, scores or packages. That cohort's packaging resumed on the new version
and its package verified. Deploys and load runs are now gated on both
instances' status (`slop_ninja_eval_ops gate`, below).

| Commit | When (UTC) | Change |
| --- | --- | --- |
| `fdb43b9b` | 04:00 | fan-out lanes; store v4 (the live store migrated in place, its four cohorts intact); queue, lane, limiter and ledger objects; scorer image `3eba00af` (binary `f7bbba5e...`, `/healthz` build identity); `max_instances` 2 -> 64 |
| `69baeab9` | 04:09 | start wait as a var, host capture, probe fixes; `max_instances` 128 |
| `e0b39580` | 04:24 (during `load-s2-r0p40-n1000` packaging) | a job's receipt objects read by key range; store v5; 60 s start bound; `max_instances` 256 |
| `edfe8afa` | 04:37 | plans parsed once per digest, lane round time; `max_instances` 375 |
| `da569b03` | 04:57 | a finished cohort's private results objects and summaries deleted from the eval bucket (store v6); `max_instances` back to the resting 64 |

The v6 purge ran on both instances in the first idle step after the deploy:
the summaries and results of the four closed load cohorts were gone from the
eval bucket (R2: key does not exist), while the sealed batches stayed.

**Application updates.** After each `max_instances` change the Container
application reported `provisioning` for about 2 minutes, and grew its pool of
prepared (`healthy`) instances with demand rather than up front (19 prepared
instances three minutes after the 256 step, 209 a minute into a 195-lane
burst). Billing for prepared instances that never run is not documented.

**M1, connections.** From one Worker invocation (`/eval/probe/connections`):

- 12 concurrent Durable Object stub calls, each held open 3 s by its object,
  all returned between 3.22 and 3.46 s.
- 12 concurrent llama-3.2-3b generations of about 100 tokens all returned
  between 0.40 and 0.59 s, one wave; a limit of 6 would have put six past
  about 0.8 s. (Mistral-small at about 100 tokens: 1.9 to 5.8 s, also no
  second wave.)

Neither AI binding calls nor Durable Object stub calls waited on the
6-connection limit. `inflight_per_lane` stays at most 6 regardless.

**M2 and M10, Workers AI rate limit** (`/eval/probe/rate`, rate-probe lanes,
8-token completions, Workers Paid):

| Probe | Calls | Rate | Throttled (429/3040) | Mean latency |
| --- | --- | --- | --- | --- |
| llama-3.2-1b and granite-4.0-h-micro, 3 min | 750 + 750 | 250 + 250 a minute (500 combined) | 0 | 263 ms, 571 ms |
| llama-3.2-1b alone, 2 min | 1,200 | 600 a minute | 0 | 184 ms |
| llama-3.2-1b alone, 1 min | 2,000 | 2,000 a minute | 0 | 191 ms |

The documented 300 requests a minute was enforced neither account-wide nor
per model up to about 6.7 times that rate for this small model, so no 429 text
could be recorded for M10, and the limiter's 290-a-minute buckets (global and
per model) are far stricter than what this account was held to. The limiter
and its throttle handling stay as the guard for capacity errors (3040) and
any enforcement the documentation describes; whether larger models or longer
outputs hit capacity limits is not measured. All five plan generators served
on 2026-10-31 at 04:10 (`/eval/probe`).


### Fan-out lanes: load rehearsals (2026-10-31)

Every run re-scored rehearsal 2's sealed n = 1,000 batch
(`cohorts/testnet-m4-rehearsal-2-k4-n1000/batch/index.json`, SHA-256
`e39189bf...`, 16 shards) on the load instance, against R_0 (the released
seed-17 package `2f88b0d2...`) and copies of it owned by synthetic,
never-registered identities, with chunks of 32, `lane_docs = 200`, a 60 s lease
and standard-4 instances. Lane counts follow the planner: W = min(n /
`lane_docs`, floor(cap / J)) lanes per job, at least 1. Each package was verified from the receipts
bucket by `eval_verify` under the load instance's own keys and by the native
validator's `package::verify` with those keys as its trusted package and
receipt roots (a private operator script).

| Run | Jobs | Lanes (W) | Cap | Commit | Scoring wall | Packaging | Chunks (attempt 2) | Receipts |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `load-smoke-r0p2-n1000` | 3 | 15 (5) | 64 | `fdb43b9b` | 181 s | 8 s | 96 (0) | 3,000 in 3 bundles |
| `load-s2-r0p40-n1000` | 41 | 123 (3) | 128 | `69baeab9` | 400 s | 419 s | 1,312 (0) | 41,000 in 41 bundles, 6.36 MB |
| `load-s2-r0p64-n1000` (faults, hold) | 65 | 195 (3) | 256 | `e0b39580` | 427 s | 52 s | 2,080 (8) | 65,256 in 65 bundles, 10.1 MB |
| `load-s2-r0p128-n1000` | 129 | 258 (2) | 375 | `edfe8afa` | 576 s | 39 s | 4,128 (0) | 129,000 in 129 bundles, 20.0 MB |

Distributions (lane reports; a cold path is the container start, the artifact
stream from R2, three 64 MiB parts at a time, and the model load):

| Run | Cold path p50 / p95 / max (n) | Cold-path CPU p50 / p95 | Chunk call p50 / p95 | Round p50 / p95 |
| --- | --- | --- | --- | --- |
| smoke | 30.1 / 36.6 / 46.7 s (15) | not recorded | 7.4 / 12.7 s | not recorded |
| R_0 + 40 | 30.0 / 43.2 / 49.6 s (122) | 23.4 / 34.7 vCPU-s | 8.3 / 13.3 s | not recorded |
| R_0 + 64 | 28.9 / 42.6 / 54.6 s (374) | 20.4 / 34.2 vCPU-s | 7.8 / 13.2 s | not recorded |
| R_0 + 128 | 30.1 / 40.9 / 50.8 s (255) | 24.3 / 32.2 vCPU-s | 10.0 / 13.1 s | 10.9 / 13.3 s (254) |

A round is one claim, the chunk call, the two R2 puts and the commit, timed
by the lane from the `edfe8afa` deploy on. Measured against the design's
projections (the private fan-out design, §6):

- **M12, round time:** 10.9 s at the median with 254 lanes, against the 9.5 s
  target and the 12 s of the 2-slot engine. The chunk call itself grew with
  the number of lanes: 7.4 s at 15, 7.8-8.3 s at 123-195, 10.0 s at 258.
- **M3 and M7, cold path:** a median of 29-30 s and a 95th percentile of
  37-43 s at every width (15 to 374 starts), against the 33 s mean and 60 s
  tail assumed. Its CPU is 20-24 vCPU-s at the median, not the 15 assumed.
- **CPU per document,** cold paths included: 1.00 vCPU-s in the smoke run and
  1.15-1.18 at 41-129 jobs, against rehearsal 2's 0.93.
- **M4, shared artifact reads:** every copy has R_0's package, so up to about
  250 lanes streamed the same artifact parts at once with no change in
  the cold path (median 30.1 s at 15 lanes and at 255). Concurrent reads of
  distinct artifacts were not measured.
- **M5, kick ramp:** 123 lanes reported within about 14 s of the dispatch and
  258 within 28 s, all through the Queue, with no stub-creation errors.
- **M8, packaging:** 129 summaries and bundles (129,000 receipts) in 39 s, no
  error. The 419 s of R_0 + 40 was the receipt-object scan fixed in
  `e0b39580`. J x n = 257k was not run.
- **Scoring wall:** the planner's formula (3 s + cold + ceil(C / W) x 9.5 s)
  gives 102 s for the smoke run, 141 s for R_0 + 40 and R_0 + 64, and 188 s
  for R_0 + 128 (§6.2). The measured walls were 181, 400, 427 and 576 s. The
  difference is the stuck first starts below, plus, in R_0 + 128, a tail: by
  345 s after queueing 4,104 of 4,128 chunks had committed (about 25 a second
  while some 250 lanes ran), and the last 24 took four more minutes. They
  belonged to jobs whose second lane was never placed, so the job's one
  running lane took its sibling's chunks in turn (a lane steals any unclaimed
  chunk of its job) while the sibling kept retrying its start (start failures
  rose from 199 to 207; three lanes never started). Four lane rows still read
  `starting` after the close; the 192 lane objects read had no alarm pending,
  and all 334 instances were `inactive`.
- **Metering:** a lane meters from its start request, so each stuck start
  counts up to 60 s of instance time.

**Stuck first starts.** In every wide run a share of the first starts were
never placed: 9 of 15, 107 of 123, 123 of 195 and about 200 of 258.
`wrangler containers instances` listed them `inactive`, with no location or
version, and they stayed so until their lane's start bound expired (60 s in
the smoke, R_0 + 64 and R_0 + 128 runs, 180 s in R_0 + 40), even after the
application's pool of prepared (`healthy`) instances had grown past the
demand (185 idle prepared instances while 200 lanes waited, 56 s into
R_0 + 128); the lane then destroyed the instance and backed off, and the
retry was placed at once. The number placed at first was close to the
prepared pool when the starts were requested (57 of 258 in R_0 + 128,
queued 5 minutes after the 375 step), and the short-lease drill below,
started a minute after the 64 step with 35 prepared instances, placed all 15
of its first starts. So the likelier reading is that starts beyond the
prepared pool at the moment of the request wait for the start bound, not
that updates alone cause it; the platform's placement is not documented. The
scoring walls above are dominated by this, not by scoring: in R_0 + 40, once
the retries were placed about 100 lanes came up together and the remaining
1,200 chunks committed in about 3.5 minutes. The cost of a wave beyond the
pool is one start bound, the deployment var `SCORER_START_WAIT_MS` (60 s),
plus the backoff; an authoritative cohort's pre-start lead
(`prestart_lead_ms`) absorbs it only if it exceeds the bound plus a cold
path.

**Faults injected in R_0 + 64** (plan `faults`):

- 4 lanes destroyed their containers with a request in flight: those chunks
  got infrastructure-error receipts and were re-run in isolation, resolving
  every document (no attribution);
- one document (job 9, task 333) crashed its container whenever it was in a
  request: in isolation it crashed alone and the restarted instance scored the
  control, so it is a `model_error` for job 9 only (`isolated_crashes` 1); the
  same task of the other 64 jobs is an ordinary prediction;
- 3 lanes stopped after storing their first chunk's results and receipt
  objects, before committing: after the lease, those chunks were reconciled
  from the stored objects with no compute (`reconciled` 3, no new attempt);
- 2 lanes' alarms threw right after a claim: those attempts were recorded
  lost and re-run (3 lost attempts in all);
- 10 % of kicks were dropped and every kick was sent twice: the sweep
  re-sent the dropped ones after 30 s unseen (lanes, summary and bundle lanes
  alike), and duplicates were no-ops: 65,000 task receipts plus exactly 256
  infrastructure-error receipts, one per task of the 8 re-run chunks;
- a 100 s hold (04:31:09-04:32:51, every claim `Paused`): 176 of 195 lanes
  paused within 22 s, the application's active instances fell from 180 to 4
  within about 40 s, the chunk count stopped at 1,118 of 2,080; 11 s after the
  release the sweep had re-kicked the paused lanes, which paid a second cold
  path each and finished the same cohort.

No commit was fenced (the lease never expired under a live claim). Both
verifiers accepted every package; the native validator was given the load
keys as its trusted package and receipt roots.

**Short-lease drill** (`load-s3-short-lease-r0p2-n1000`, R_0 + 2, 15 lanes,
`lease_ms` 8,000, the `da569b03` deploy, 04:58-05:26). Only load plans may set a
lease under 30 s. A scorer request is bounded by the claim's lease less 15 s,
at least 5 s, so here every 32-document call (8-10 s) outlived its 5 s bound:

- 93 of 96 chunks timed out; each lane destroyed its container and the chunk
  got 32 infrastructure-error receipts (6,040 receipts in all: 3,000 task
  receipts and 3,040 infrastructure-error receipts for the 93 timed-out
  attempts and 2 lost ones);
- the isolation runs then scored every document alone on a fresh instance
  (about 1 s each, inside the bound), so 91 chunks finished at attempt 2 and 2
  at attempt 3, with no document attributed (`isolated_crashes` 0, `failed`
  0); the three jobs' mean Brier matched R_0's within 3 x 10^-8;
- the sweep's staleness bound for a scoring lane is 3 x `lease_ms`, 24 s here,
  shorter than an isolation run, so busy lanes were re-kicked 170 times and
  superseded: 107 starts for 15 lanes;
- one lane stayed in a single alarm invocation for more than 13 minutes (its
  next alarm 811 s overdue) while holding its job's last chunk, so the job and
  the cohort waited about 10 minutes for it; the lane loop checks its 600 s
  step budget and supersession only between chunks, and an isolation run is
  one chunk. The cause of that invocation's length was not determined;
- scoring took 1,648 s, against 181 s for the same cohort at a 60 s lease.
  No commit was fenced. Both verifiers accepted the package;
- the drill was still scoring (until 05:25:42) when authoritative cohort 5
  began generating (05:25:03) and pre-started its scoring lanes (05:25:16),
  against the design's rule that no authoritative cohort generates or scores
  during a load run. A load run is now gated on the main instance having no
  cohort queued, in admission or at work.

**M6, Durable Object CPU.** A Worker tail captured during the first three
minutes of R_0 + 128 (private) separates invocations, not the
cohort object's requests: lane `/kick` handling took 7 ms of CPU at the median
(15 ms at most, 268 kicks), a lane alarm invocation 113 ms of CPU at the median
over about 63 s of wall time (2 of 200 `canceled`), and the load instance's
status 15 ms. No invocation failed on CPU or memory limits. The cohort object's
CPU per claim and commit was not measured separately.

**Not measured.** M9 (whether the 1-vCPU type fits), M11 (whether a container
survives between two alarm invocations: no lane's work outlasted its 600 s
step), packaging at J x n = 257k (M8), concurrent reads of distinct artifacts
(M4), a fenced late commit (only native tests exercise it), a redeploy in the
middle of scoring (only incidentally: R_0 + 40's packaging resumed after the
`e0b39580` deploy), and placement in a chosen region.

**Copies of R_0 are not bitwise identical across lanes.** In the smoke run
the two copies' predictions differed from R_0's by an RMS of 3 to 6 x 10^-8 in
involvement, and their mean Brier by 6 x 10^-10 and 1 x 10^-9. Every lane
reported the host CPU model `AMD EPYC` (the string the instances expose), so
the cause is not visible there; ONNX Runtime kernels that differ across CPU
generations remain the likely explanation (not verified). Under
`testnet-reward-v0`, whose row pays any entrant scoring above the baseline,
the copies that scored above it took the whole row: 2 of 2 copies in the smoke
run, 33 of 40 in R_0 + 40 and 8 of 64 in R_0 + 64 (where job 9's model error
put it below), with nothing burned. In R_0 + 128 no copy scored above R_0 (51
equal, 77 below), so the whole row went to uid 0, R_0's owner in the load
plans. Which side of the baseline a copy lands on is therefore decided by
numerical noise. reward-v1's inclusive 1 bp near-equal drop and near-duplicate
clusters would give them nothing; `components.json` already puts every copy in
one cluster (RMS at most 0.01). The design's lane-layout invariant and
`docs/a-container-serving.md` now state this difference; making copies tie
under `testnet-reward-v0` was not done (review fixes, finding 6).

### Fan-out lanes: authoritative cohort 5 (2026-10-31)

`testnet-fanout-rehearsal-5-k3-n40` (plan v2, n = 40, R_0 and one entrant,
chunks of 4, `lane_docs` 8, a 60 s lease, a 60 s pre-start lead), on the
`da569b03` deploy (admission opened at block 8339799), ran its bulk steps as lanes:

| Step | When (UTC) | Notes |
| --- | --- | --- |
| generating | 05:25:03.6 | admission minimum reached; humans drawn 1.4 s later |
| pre-start | 05:25:16 | the 10 scoring lanes kicked during the generation tail |
| sealing, scoring | 05:25:57.6, 05:25:58.1 | 4 generation lanes made 25 calls (p50 3.9 s, p95 22.4 s, 0 throttles) |
| packaging | 05:27:30.0 | 10 lanes, 11 starts (cold path p50 27.6 s), 20 chunk calls |
| transition | 05:27:38.2 | package, target and receipts durable; 104 receipts in 2 bundles |

The package verified under the
T12 roots pinned in `testnet-trust.json` with `eval_verify` and with the
native validator's `package::verify` under validator2's trusted keys
(a private operator script). R_0 and the entrant scored the same Brier,
so the row went to uid 0.

Stuck starts recurred at this width: 4 of the 10 first starts were not
placed (`container did not answer`), and their lanes were placed on retry.

One defect showed up only here, since load cohorts have no generation and
so no pre-start: 6 attempts were lost (24 infrastructure-error receipts), one
for each lane whose pre-start succeeded. The code path accounts for it: such
a lane read its assignment during generation, before the manifest existed,
and a chunk needs the manifest digest its receipts are bound to, so the
lane's first claim was left to expire; after the 60 s lease the chunk was
reconciled, recorded lost and re-run, spending one of its 3 attempts. The
branch now reads the assignment again when a pre-started lane's first claim
arrives, and hands the claim back with no attempt spent if the manifest is
still missing. That change was deployed with the review fixes below
(`727307ce`, during cohort 5's transition, which does no bulk work).

**Spend.** Each cohort settled its measured spend in the ledger (the
ledger's state is in the private operator inventory). The probes' Workers AI
and scorer-probe use is not charged to the ledger. All 277 scorer instances
the application listed were `inactive` afterwards.

### Fan-out review fixes and re-runs (2026-10-31)

A review of `bec035ab` raised eight findings. Each was checked against the
code and the stored evidence and confirmed (finding 5 by reading the code).
Seven are fixed and finding 6 in part (README, "Hidden evaluation";
the private fan-out design, §14):

| # | Finding | Fix |
| --- | --- | --- |
| 1 (high) | An isolation run was one call inside one alarm invocation, so it could outlast the 15-minute wall, die uncommitted and spend every attempt, ending the cohort `unresolved_infrastructure` (the drill's 13-minute invocation) | Resumable isolation: each document's outcome is recorded as it resolves (`/lane/iso-progress`, which renews the lease); before every step the lane checks the plan's `step_budget_ms` and whether it was superseded, and hands an unfinished attempt back (`/lane/iso-yield`); the next claim continues the same attempt number and never recomputes a recorded document |
| 2 (high) | Failures before any document reached the model (shard read or digest, missing manifest, a first restart) spent attempts and wrote receipts; reconciliation read any R2 error as absence | `/lane/give-back` for every failure before dispatch: no attempt, no receipt, a scorer failure counted, so an outage pauses (`scorer_unavailable`, resumable). Reconciliation hands read errors back for a retry, records lost only on a confirmed absence, and re-stores a missing receipt object from the durable results |
| 3 (medium) | A retried `/plan` for a queued label deleted its reservation; open reservations left headroom at a billing-cycle boundary | A queued label never touches the ledger again (the same bytes answer `already`); a refused queue releases only a reservation its own request created; open reservations count in every cycle |
| 4 (medium) | One failure in isolation was attributed; any failed request was a crash; the single-document limit came from the lease | Only container death is a crash (stopped, or `/healthz` down, after the call; or the declared `doc_timeout_ms` exceeded); a document that kills its instance runs again alone on a freshly restarted instance that has just scored the control, and is a model error only if it kills that one too; a failure with the container up is an infrastructure error; the scorer aborts on any panic; `components.json` publishes the corrected `crash_rule` |
| 5 (medium) | An artifact that never loads was never attributed; the load wait, start-failure count and step budget were compiled in | Artifact failures on fresh instances (a load that fails or outlasts `artifact_max_mib` x `load_ms_per_mib`, death during the load, missing or rejected parts, a control a fresh instance cannot score) make an entrant's unresolved documents model errors after `artifact_failures_before_model_error`; an oversize or mismatched manifest at once; references are never attributed and pause instead. All are plan parameters |
| 6 (low) | Copies of R_0 split the row by lane noise | The invariant is qualified (design §1, `docs/a-container-serving.md`) and `slop_ninja_eval_ops copy-ties` reports ties per run. Making copies tie was rejected for this branch: quantizing cannot guarantee ties, ONNX Runtime has no supported way to pin x86 kernels across hosts, and a tie tolerance would change `testnet-reward-v0`'s row, which validators recompute, a protocol change; reward-v1's near-equal drop is the planned remedy |
| 7 (low) | The deploy and drill records overstated the evidence | Corrected above; `slop_ninja_eval_ops gate` refuses a deploy while either instance has a cohort generating, sealing, scoring or packaging, and a load run also while the main instance has one queued or in admission |
| 8 (low) | The rehearsal tooling was untracked Python | `examples/slop_ninja_eval_ops.rs`: load plans (it reproduces the stored plans' content), the gate, status, tail and copy-tie summaries and the verification helpers; the private load-verification script now uses it |

One more defect showed up in the re-runs and is fixed: a cohort already
faulted logged `fault:...` again on every repeated failure (four entries for
one pause in run B).

**Deploys**, each gated on both instances' status (cohort 5 was in
`transition` throughout; `max_instances` stayed at 64):

| Commit | When (UTC) | Change |
| --- | --- | --- |
| `727307ce` | 06:30 | the review fixes; store v7 (both instances migrated in place); scorer image `d878be5d` (binary `9efb681a...`, aborts on panic) |
| `0d48e767` | 06:32 | the `crash_tasks` load fault |
| `cb3e2098` | 06:44 | hand-back and attribution events per job on status |
| `d6ffc3d9` | 06:45 | a failed isolation hand-back as its own event |
| `1ee67d4e` | 06:51 | a cohort fault logged once |

A scorer probe at 06:31 passed the new image's build identity (start 0.6 s,
stream and load 22.8 s).

**Re-runs** on the load instance, as above: rehearsal 2's sealed n = 1,000
batch, chunks of 32, `lane_docs` 200, R_0 and copies owned by synthetic
identities, plans written by `slop_ninja_eval_ops load-plan` (`step_budget_ms`
600,000, `doc_timeout_ms` 60,000, `artifact_max_mib` 640, `load_ms_per_mib`
150, `artifact_failures_before_model_error` 3). Each package verified with
`eval_verify` and the native validator's `package::verify` under the load keys.

| Run | Jobs (lanes) | Faults | Scoring wall | Chunks at attempt 2 (3) | Receipts |
| --- | --- | --- | --- | --- | --- |
| `load-r1-review-faults-r0p4-n1000` | 5 (25) | lane 3 killed once; job 1 tasks 330-333, 335 and 336 kill the container on every request, task 334 once alone; job 2 kills it on every request, the control included; job 4's load fails | 500 s | 7 (0) | 5,224 |
| `load-r2-review-r2-outage-r0p1-n1000` | 2 (10) | every batch-shard read fails for 120 s after scoring starts; `failures_before_pause` 6 | 219 s (105 s paused) | 0 (0) | 2,000 |
| `load-r3-review-short-lease-r0p2-n1000` | 3 (15) | `lease_ms` 8,000: the short-lease drill again | 710 s | 71 (0) | 5,272 |

Run 1 (findings 1, 4 and 5):

- Job 1's chunk 10 crashed at attempt 1 (32 infrastructure-error receipts).
  In isolation each of the six documents killed its instance, then killed a
  freshly restarted instance again right after it scored the control: six
  model errors (`isolated_crashes` 6), all at attempt 2. Task 334 killed its
  instance once, then answered on the proven instance: a prediction, not
  attributed. The run was handed back once at the step budget (`iso_yield` 1
  for job 1) and finished under the same attempt; no chunk reached attempt 3.
- Job 2 crashed on everything: its lanes' first chunks crashed (160
  infrastructure-error receipts), and in isolation the control killed each
  freshly restarted instance. After three such artifact failures (two
  hand-backs) at 06:36:01 its artifact was recorded failed (`control document
  failed on a fresh instance`) and its 1,000 documents were committed as
  model errors with no container.
- Job 4 never loaded: three artifact failures on fresh instances, then at
  06:34:31 `artifact load failed (load fault)` and 1,000 model errors at
  attempt 1.
- Job 0's killed chunk resolved in isolation with nothing attributed; job 3
  scored 1,000 predictions. No attempt was lost, no commit fenced, and the
  cohort closed with no fault. 19 of 33 starts were not placed at first
  (`container did not answer`), as in the earlier wide runs.
- The same `/eval/load/plan` POST sent twice answered `{"already": true,
  "queued": 6}` the second time, and the ledger kept its one reservation
  (finding 3).

Run 2 (finding 2): 29 s into scoring, after six claims were handed back
(`given_back` 6), the cohort paused (`scorer_unavailable`, 06:46:08). Every
chunk was still at attempt 0, with no request made and no receipt written.
After the outage window `/eval/load/clear` (06:47:53) resumed the same
cohort, which finished scoring 85 s later with exactly one receipt per task
per job, all at attempt 1. The copy scored 4.9 x 10^10 (of 10^18) above R_0
and took the whole row (`[[1, 65535]]`), as finding 6 describes.

Run 3, against the first short-lease drill (1,648 s, 170 re-kicks, 107
starts, 2 lost attempts, 2 chunks at attempt 3, one invocation over 13
minutes): 710 s, no re-kick, 91 starts, no lost attempt, nothing past
attempt 2. 71 of the 96 chunk calls outlived their 5 s bound (chunk call p50
4.2 s here); their isolation runs were handed back 8 times at the step budget
and 5 times after a restart that failed, and continued; nothing was
attributed. With the staleness bound now three times the larger of the lease
and `doc_timeout_ms` (180 s), no busy lane was re-kicked.

**Spend.** The three re-runs and the probe each settled their measured
spend in the ledger. All 277 listed instances were `inactive` afterwards.

**Not re-run live.** The billing-cycle boundary (the next is 2026-11-01), a
lane killed in the middle of an isolation run, a manifest over the size
bound and a reference artifact that never loads are covered by native tests
only.

### Reward-v1 rehearsal (2026-10-31 to 2026-11-01)

Branch `subnet-testnet/reward-v1-live`. The eval Worker's main instance
queues only v3 and probe plans (store schema 8, migrated in place from 7);
the coordinator verifies `reward-v1` bundles and commits `WeightOperationV2`
identities. Both Workers were deployed once, before the probe build, with
both eval instances idle, and not again during the rehearsal. The scorer
image was not rebuilt, so its binary pin is the one the fan-out rehearsals
used.

**Reference chain.** The rehearsal's `R_0` is `r0-flat`, the released `R_0`
with its involvement calibration flattened halfway to 0.5, so that the
released model can beat it by the margin. Before announce the chain must
restart at the released `R_0` (a fresh authority, or a reset of
`reference_state`, the published index and the reuse ledger together).

**Pool.** `testnet-human-pool-pc1`: 4,593 humans from the public-candidate
export (`pool-public-candidate`: rows whose every experiment role was
consumed Test, held or diagnostic, one record domain and source group, at
most 1,000 tokens), in 72 private shards beside the manifest. Like pool v1,
the texts are exposed by design (`testnet_plumbing_only`).

**Artifacts.** Built from the released `R_0` (seed 17) by editing its small
files; the graph parts are shared:

| Name | Change | Aligned distance on 180 local documents |
| --- | --- | --- |
| `r0-flat` | calibration levels `y -> 0.5 + 0.5 (y - 0.5)` (rank-preserving) | 0 from `R_0` |
| `flat-near` | the same flattening with 0.52 in place of 0.5 | 0 from `r0-flat` |
| `ha-0.5`, `ha-2.0` | Gaussian noise of 0.5 or 2 standard deviations on three head weight matrices | 0.022 and 0.061 from `R_0` |
| `soup` | mean of the head weights of `R_0` and `ha-2.0` | 0.034 from `R_0`, 0.029 from `ha-2.0` |
| `h-near` | `ha-2.0` with its calibration flattened to 0.97 (rank-preserving) | 0 from `ha-2.0` |
| `fail-all` | origin weights at the float limit: every output overflows | (no prediction) |
| `no-load` | a truncated tokenizer: the artifact never loads | (no prediction) |

Synthetic owners are never-registered identities (UIDs 201 to 209); their
units burn through the owner hotkey's UID. Miner1 (UID 3) and the
coordinator hotkey (UID 1, with a stale registration block, so its units
burn) own the others. This departs from the rehearsal plan's rule that no
row pays our own miner: the released `R_0` was given to miner1 because rtao
holds a row whose positive entries are only the owner hotkey's UID, so
validator2 needed a positive non-owner row to commit at all. Cohort 8's
target therefore paid miner1, and both signers re-committed it each tempo
through epoch 54. Later rehearsals give a credited artifact a synthetic
owner unless the drill needs a committable row.

**Plans.** Every v3 plan: n = 1,000, mix 20/55/25, K = 4 tempos, admission 1,
transition 2, 3 attempts, `sigma` 0.035 (whitepaper 0.57, Section 7), so
`delta` = 0.07, a lease of 8 tempos, rewrite-source reuse of at most 4 parent
uses 7 cohorts apart, probe v1 (1,024 documents), canary audit draws of 100
humans, 20% generation spares, and the testnet provisional gates: operating
point 0.5, loose false-positive limit 10%, strict tier 5%, share MAE 0.30,
attribution 0.

| Cohort | Roster | Opened, admission closed, published, closed (epoch) | Generation | Scoring | Receipts | Outcome |
| --- | --- | --- | --- | --- | --- | --- |
| 6 `testnet-rv1-probe-v1` (probe build) | `r0-flat` | 36 (drawn at once) | 1,019 calls, 6.6 min | 1 job, 6 lanes, 1.4 min | (probe record) | probe v1: 205 / 563 / 256 documents by largest remainder; `r0-flat` fingerprinted |
| 7 `testnet-rv1-a` | `r0-flat` alone | 37, 38, 39, 41 | 988 calls, 6.3 min | 1 job, 5 lanes, 1.5 min | 1,000 | `NoTarget` (`S_R0` 0.8613): the launch burn row stays |
| 8 `testnet-rv1-b` | `r0-flat`, 7 entrants | 41, 42, 43, 45 | 995 calls, 6.8 min | 7 jobs, 35 lanes, 4.6 min | 14,344 in 7 bundles | `Margin`, no promotion (below) |
| 9 `testnet-rv1-c` | `r0-flat`, 5 entrants | 45, 46, 47, 49 | 997 calls, 6.5 min | 3 jobs, 15 lanes, 3.5 min | 5,448 in 3 bundles | `NoTarget`: cohort 8's target stays active |

Every cohort accepted 960 generations (the 550 mixed and 250 model-only
documents plus 20% spares) and sealed exactly 200 / 550 / 250. The draws
took 200 fresh negatives; mixed parents were 199, 197 and 199 same-cohort
negatives, 0, 13 and 17 earlier passages (rows of the previous reward-v1
cohort that it drew but parented nothing from: the ledger counts a
never-parented row as eligible at once, and the reuse ledger starts with
the reward-v1 draws, so no pool-v1 passage was a candidate) and the rest
fresh source-only passages. Stuck starts recurred: 28 of
cohort 8's 30 scorer starts and 8 of cohort 9's 15 answered `container did
not answer` once and were placed on retry.

**Cohort 8** (`S_R0` 0.8633, cutoff 0.8728, `w` 0.0096):

| Entrant (owner) | Score | Distance to `R_0` (batch) | Result |
| --- | --- | --- | --- |
| released `R_0` (miner1) | 0.8821 | 0 | gated; outscores `r0-flat` by more than `w / 2` on the batch and the probe, so not a copy; the only cluster head; the drill flagged it on audit draw A and again on draw B (`canary.drill` twice): blocked; strict tier passed |
| `ha-2.0` (201) | 0.8411 | 0.032 | gated, below the cutoff |
| `r0-flat` copy (202) | - | - | `public.content`, never executed |
| `flat-near` (203) | 0.8658 | 0 | `public.near.current` |
| `fail-all` (204) | 0 | - | every document a model error; `share.mae_above_threshold`, `human_fpr.loose` (a failure counts as a false positive) |
| `no-load` (205) | 0 | - | three artifact failures on fresh instances, then model errors; the same two gates |
| `ha-0.5` (coordinator hotkey) | 0.8692 | 0.012 | outscores `r0-flat` by more than `w / 2` on the batch but not on the probe: `public.near.probe` against `R_0`'s fingerprint |

No other gated head reached the cutoff (`ha-2.0` fell below `R_0` on this
pool, against 0.928 and 0.933 on the local documents), so nobody was
promoted: the reference chain stayed at `r0-flat`, and the credit stood, 1/1
to the blocked head's cluster, 65,535 units to UID 3. The plan's alternative
(the next head promoted after two flags) needs a second artifact above the
margin and distinct from every published one; none of these variants is, so
that branch, the fallback (which needs `R_1`) and a clean re-audit are
covered by the protocol and coordinator tests only.

**Cohort 9** (`S_R0` 0.8666): `soup` (miner1) 0.8659 at distance 0.020 from
`r0-flat`, `public.near.current`; `h-near` 0.8522, at 0.034 from `r0-flat`
on the batch, `public.near.probe` (the archive search matched `ha-2.0`'s
fingerprint); exact copies of the released `R_0`, `fail-all` and `no-load`,
published by cohort 8, `public.content`, never executed. Nobody reached the
cutoff, so the single-flag drill had no candidate.

**Verification.** `reward_v1_verify` verified every package from the
receipts bucket under the pinned roots (every receipt cut from its chunk,
the published index against the manifest, the recomputed outcome) and each
closure (the index after it rebuilt from its fingerprints: 1 content after
cohort 7, 7 after cohort 8 and 9 after cohort 9; cohort 9's closure left the
reference chain at `r0-flat` and cohort 8's target active). The native
validator (validator2's configuration) verified the launch bundle and cohort
8's bundle, both directly and carried by cohort 9, including all 14,344
receipts each time.

**Signing.** The coordinator committed the launch burn row `[[0, 65535]]` at
epochs 37 to 44, each revealed with the chain's weights equal to the row.
Cohort 8's target activated at epoch 45: signing was paused for the
boundary, the coordinator form of its bundle (14,471 bytes, without the
receipt index) was installed after the closure, and signing resumed; the
epoch-45 row `[[3, 65535]]` was committed and revealed matching. validator2
cannot commit the launch row: rtao holds a row whose positive entries are
only the owner hotkey's UID (`row_would_be_held`). With cohort 8's validator
bundle (6.4 MB: the receipt index and 7 chunk objects), validator2 derived
`[[3, 65535]]` and committed its own timelocked commit (commit-reveal v4) at
epoch 45, block offset 320, under the operation for epoch 45 issued only
after the authorization (lease 45 to 53) checked. The chain revealed it at
epoch 46 and the weights read back equal to the row; the epoch-46 re-commit
was queued under the same check. rtao's reveal watcher took about 45 minutes
to replay the blocks between the commit and its reveal, as in M2.

Cohort 9 produced no target, so cohort 8's target is carried: once cohort 9's
package existed (epoch 46), a reward-intent authorization naming cohort 9's
manifest and package (`--carrier`, lease 46 to 54) replaced the first one on
the coordinator (22,696 bytes with the carrier's records), whose epoch-47
row was committed under it. validator2 was stopped after its epoch-46 reveal
read back (the journal pauses on the signal), restarted on the carried
validator bundle, adopted the queued epoch-47 operation of the same target
and committed it under the carried authorization; it revealed at epoch 48
with the weights equal to the row. Both keep re-committing cohort 8's target
each tempo until the lease ends at epoch 54; after that the coordinator
defers (`authorization_refused`) and validator2 stops (`tempo_refused`) until
a new bundle is installed.

**Rows.** Each side's row digest binds its own snapshot block. Every reported
digest was reproduced offline from the verified package and a snapshot at
the same block (`reward_v1_verify --snapshot-at`): the coordinator's at
epochs 37, 38, 45, 46 and 47 and validator2's four derivations (the launch
dry run, both cohort 8 runs and the carried run). So both sides compute the
same canonical row from the same snapshot, and the rows both committed at
epochs 45 to 47 had identical entries, `[[3, 65535]]`.

**Spend.** The probe build and cohorts 7 to 9 each settled their measured
spend in the ledger, including one reservation made before a billing-cycle
boundary and settled after it, as the reservation rule requires.

**Defect found.** `reward_v1_verify` checked the validator bundle against
the coordinator's one-stored-value bound and refused cohort 8's (fixed
before the install; the native validator's own file bound is 64 MiB).

### Reward-v1 review fixes (2026-11-01)

Two reviews of `9becd961` raised 35 findings (8 high). Each was checked
against the code and the stored evidence and confirmed; four repeat others
(16 is 5, 17 is 6 and 9, 18 is 2, 24 is 8 with the receipt bound) and one
reported no defect (19). The fixes:

| # | Finding | Fix |
| --- | --- | --- |
| 1 (high) | `finalize` appended a public-copy failure (gate 5) after a loose false-positive failure (gate 6), so the package failed its own order check and the coordinator retried it every tick | Gate failures are kept in gate order; a test combines every recorded gate with every public-copy reason. A package build that refuses its own inputs faults the cohort (`package_refused`, resumable after a corrected deploy) instead of retrying |
| 2, 18 (high) | The active target was rebuilt from the bundle itself, so a launch bundle after an activation, or a replaced cohort target, still committed | `target_bundle::verify` takes the holder's own active target. The native validator records it in `<journal>.held-target.json` and the coordinator in `sign_held_target` (journal v7), only from checked closures and only forward; a bundle must carry the held target (same activation epoch) or its successor, and the launch bundle only while nothing has activated |
| 3 | Validators trust the coordinator's published index | A carrier's index must extend the target cohort's index and contents; full rebuilds are listed as not implemented (README) |
| 4 | A carrier package's outcome was not recomputed | The carrier's published index and receipt index travel in the bundle (and its receipt chunks for validators); its outcome is recomputed and must be `NoTarget` |
| 5, 16 | The native validator re-committed its startup row each tempo | Each tempo's row is derived afresh from a finalized snapshot and owner hotkey, under the authorization checked for that epoch; each operation keeps its own row for its reveal check |
| 6, 9, 17 | Burn-only and deferred rows, and a lapsed lease, stopped the native validator for good | They defer the tempo: exit 75 at startup (systemd retries later, reading the bundle again), a skipped tempo inside a dispatch run. While the row is all burn a native validator commits nothing and its previous weights stay on chain (README); the paper's statement of this is still to be added |
| 7 | The manifest could not lower the near-copy distance `d` | `0 < d <= 0.025` is a manifest parameter; the other reward-v1 values stay fixed by the rule |
| 8, 24 | The coordinator's target route took 1 MiB while a reward-v1 bundle may be 2,000,000 bytes, and the embedded published index grows without bound | The route takes either bundle form up to its own bound; binding the index by digest (R2 or chunked rows) is not implemented yet (README) |
| 10 | The span graph is allow-listed but not served; `artifact_max_mib` is a tuned plan value | Recorded (README); serving span heads, a schema-derived bound and admission wiring are still to do |
| 11, 32 | Compiled limits | The batch size is bounded by the receipt index's sequence bound (receipts of every job at every attempt), attempts by the receipt's attempt number, generators by the price table (distinct), strata by the sequence bound |
| 12 | Every entrant scores both audit draws; draw A was smaller than the loose tier's sample | Draw B is taken only when the plan lists a drill (the testnet canary has no anchors); draw A must hold at least the cohort's humans. Scoring audits for candidates only is not implemented |
| 13 | A re-audit could reuse its draw; the gap counted operator cohort numbers; promotion walks only retained heads; no-target cohorts cannot authorize the burn row | A re-audit must name another draw; the gap counts drawn reward-v1 cohorts (reuse ordinals, store v9); the other two are recorded (protocol README) for the paper |
| 14 (high) | Rows summing to 65,535 were committed as they are, but rtao and the chain store them max-upscaled | `WeightRowV2::chain_values` (exact, half to even) is what both signers commit and compare with reveals; the canonical row and digest stay the audited record. Agreement with rtao's arithmetic is tested over every two-entry row |
| 15 (high) | A pending re-commit wedged target switches, missed tempos and re-derived rows | Retained work is settled before enqueueing: an operation that never landed is retired (journaled `slop_ninja_retired` and rtao's hand-off mark), a re-derived row takes the next label, anything else defers (`pending_operation_unsettled`) |
| 20 (high) | Nothing stopped a cohort drawing fresh human stock | Only pools listed in `EXPOSED_POOLS` are drawn (pool v1 and the public-candidate refill); a plan naming another pool is refused, and the draw faults (`fresh_stock_refused`). From exposed pools a row may be drawn again: never-drawn rows first, never twice in one cohort, the reuse limit and gap kept for parents |
| 21 (high) | The rewrite ledger never recorded generators, and the prompt had one voice and operation | Every parent use records its voice and operation at the draw and its generator when the child is accepted; a reused passage avoids each (`rewrite_voice_operation_v1`: six voices, six operations) |
| 22 (high) | Ledger state, cap and fee lines in the runbook and the cap value in `wrangler.toml` | Moved to the private inventory; the cap is passed at deploy time; the branch lands on main as one squashed commit |
| 25 | Receipt retention unimplemented, and the README rule was wrong | README corrected to the whitepaper rule; deletion is not implemented |
| 26 | Stock consumption | Audit humans join the reuse ledger; draw B only with a drill; exposed rows are reused. The R2 `sources` record is not read back (a new authority starts its ledger empty, as the announce restart intends) |
| 27 | The gap counted operator numbers and K, G were not bound | Ordinals as in 13; K, G, their unit and the prompt set are in the policy the manifest binds by digest and in `components.json` |
| 28 | Spend cap | Explicit: a testnet deployment without a cap refuses reservations, mainnet has none; the cap is passed at deploy time |
| 29 | Rehearsal reuse text; no pool column | Text corrected above; the ledger records each row's pool and candidates come from the plan's pool |
| 23, 30, 31, 33, 34, 35 | Branch history, private inventory, dirty-tree deploys, the miner1 deviation, commit trailers, file modes | Inventory and file modes fixed privately; the deviation is recorded above; Workers are deployed from a clean pushed commit (the deploy script refuses a dirty tree); other branches' history is not part of this snapshot |

**Re-runs after the fixes (2026-11-01).** Both Workers were deployed from
the clean pushed commit `809ec118` while both eval instances were idle
(`slop_ninja_eval_ops gate` clear): the eval Worker first (store v8 -> v9 in
place on `main`; container application unchanged), then the coordinator with
signing paused (journal v6 -> v7 in place). For about a minute after the
coordinator deploy its Durable Object still ran the previous code, so a launch
bundle sent in that window was installed by the old code; signing was paused,
so nothing was committed, and the next status read showed journal v7. Wait
for the schema version to change before acting after a deploy.

- **Held targets.** The carried cohort-8 bundle, rewritten in the new form
  (cohort 9's published index and receipt index; its outcome recomputed as
  `NoTarget`; 5,448 carrier receipts verified in the validator form), was
  installed on the coordinator, which recorded cohort 8's target as held
  (activated at epoch 45). The launch bundle was then refused (`launch
  authorization after activation`), and the coordinator committed epochs 52
  onward under the carried authorization, its rows now carrying the committed
  form. validator2 ran the new binary: the dry run recorded the same held
  target (`<journal>.held-target.json`, mode 600) and verified 19,792 receipts;
  a dry run on the launch bundle was refused (exit 3); dispatch adopted the
  retained epoch-51 operation of the same target, committed it, and its
  reveal read back equal to the committed row. Each later tempo's row was
  re-derived from that tempo's finalized snapshot.
- **Fresh stock.** A plan naming another pool was refused at queue time with
  the fresh-stock error.
- **Cohort 10 `testnet-rv1-d`** (`r0-flat` and an exact copy of a published
  artifact; opened 52, packaged 53): drawn in exposed mode at reuse ordinal 4
  (cohorts 7 to 9 migrated to ordinals 1 to 3); 200 negatives, audit draw A of
  200 and no draw B (no drill); mixed parents 199 same-cohort, 13 earlier and
  448 fresh, each with a recorded voice and operation from
  `rewrite_voice_operation_v1`. Generation had more length rejections than
  the single-prompt cohorts (64 length, 3 markup, 1 preamble of about 1,030
  calls; the 20% spares covered them). The copy was never executed
  (`public.content`); `r0-flat` scored 0.8460 on 1,000 receipts; the package
  is `NoTarget` and verified offline (outcome, receipts, index binding, and
  the reuse limit, gap unit and prompt set in the policy the manifest binds).
  As a no-target cohort it carries cohort 8's target: bundles naming cohort
  10 as the carrier (lease 53 to 61) were verified and installed on the
  coordinator and on validator2.
- **Not re-run live.** A row paying two UIDs (finding 14): no artifact
  available beats `r0-flat` by the margin while staying clear of every
  published one, so no live cohort can produce one yet; the committed form is
  covered by the protocol, validator and coordinator tests. Promotion,
  fallback and canary re-audits stay covered by tests, as before.

### Arena migration review fixes (2026-11-02)

A review of the arena migration (`5301072a`, then `1eb12097` on main)
raised 27 findings. Each was checked against the code at main's head. Eight
were already fixed in `1eb12097` (Observe mode, encoded replies, streamed R2
reads with per-kind bounds, bounded internal bodies, the alarm export, the
seal size, the runtime home, the recycle removed); the rest:

| # | Finding | Outcome |
| --- | --- | --- |
| 1, 14 | An unauthenticated client holding `/hook` bodies open reserved about 12 pages and a glue ticket each before any byte, and could starve signed deliveries and the cron of tickets | Fixed. A `/hook` request with no signature header naming the receiver's subscription is shed in JS (401) before any ticket or scope; the Rust receiver checks every header field before any body byte (`webhook::precheck`); a body is admitted only as its bytes arrive (the exact length's block, or a buffer grown with the bytes received); a body still incomplete after the provider's 5 s acknowledgement window is cancelled (408). The hook's abandonment deadline is now the sum of its bounded awaits. Not done: glue tickets reserved for the cron and Durable Object entries (Enforce prerequisite, design section 8) |
| 2 | Delivery continuations took the signer claim for steps they could not finish (a metadata fetch never fits, a submission rarely), and abandoned it | Fixed. The Worker sends its remaining budget; the journal hands out a step only when its worst case (`signing::step_worst_ms`, shared by both sides) fits, else releases the claim with nothing pending, no RPC charge and no drill taken (`sign_budget_declines`) |
| 3 | Continuation awaits had no deadline, so an overrun could abandon its scope and ticket | Fixed. Every Durable Object call on the delivery, cron and signing paths carries `AbortSignal.timeout` (verified for stubs in local workerd); each kick and journal call starts only while its allowance fits the budget; `/status` reports an arena `fault` for abandoned scopes or a poisoned pool |
| 4, 20 | `select` + `Delay` timeouts left pending futures in the cron, liveness, status and Workers AI paths | Fixed. `within` is gone: Durable Object calls use `AbortSignal.timeout`, and R2 reads and writes and Workers AI runs are raced against their deadline in JS (`cf_io::settle`), so the awaited promise always settles. The lane objects (`cf_lanes.rs`) still use it (step 2 remainder) |
| 5, 22 | Spilled glue was classed `Capacity`; internal routes answered 400 to an arena refusal | Fixed: spilled glue is classed as freed by waiting (`Retry`); an unreadable internal body answers its own status (503), a malformed one 400. Not done: the homes opened at a fixed end of the pool (Enforce prerequisite) |
| 6 | A home refused at first use stayed disabled for the instance's life | Fixed: a refused open is counted (`home_open_failures`) and retried at the next use |
| 7 | Observe on a fixed memory turned over-use into a trap (two package steps of co-resident objects) | Fixed in part: the seal and package steps are admitted physically (`ScopeRef::child_physical`), so a step the pool cannot serve even scattered waits (`Retry`). Not done: the lane objects' R2 reads bounded by object kind (step 2 remainder) |
| 10, 13 | A fixed 96 MiB linear memory left the heartbeat Worker about 27 MB for JS, unmeasured; one model for both roles | Fixed in part: the `Observe` build links growable memory under the same ceiling, so each isolate holds the linear memory it uses. Not done: the step-0 JS and stack measurement and per-role models (design section 8, step 0) |
| 11 | The cohort-step R2 bound (4 MiB) is not from any schema; a pool-v2-scale manifest would fault the draw | Open (step 3 remainder): byte sizes in the pool schema or plan, a pool-v2 export the draw can hold, and a draw test at the AR-8 shape, with the plan and pool owners. Cohorts draw only exposed pools until an outside entrant is admitted |
| 12, 18 | The package and seal footprints are fitted to the fan-out code, not `package_v3`/`seal_v3`; witness gaps | Open (step 0 and step 2 remainder). The cohort steps now report per-kind peaks on their own routes (`cohort/draw`, `cohort/seal`, `cohort/package`, `cohort/load-seal`, `cohort/close`), measured live below; the v3 seal at n = 1,000 used 419 of its fitted 430 pages |
| 21 | Whole-body `Response::json` reads of Durable Object replies | Fixed: replies are read with the bounded readers, bounded by the holding route's control footprint |
| 23 | Footprint constants without a derivation | Fixed for the state home (`GLUE_RESIDENTS` objects at 512 B each). `LANE_BODY_BYTES` and `STEP_OBJECT_BYTES` stay open with finding 11 |
| 24 | The runtime-gate lint did not scan the arena crate | Fixed |
| 25 | The design kept the cancelled migration steps as the plan | Fixed: design section 8 lists the remaining steps and the two-level receipt index |

**Live checks** (both Workers redeployed from main `3f9848fd` on
2026-11-02 at 01:52Z; content-free `/status` snapshots every 20 to 40 s
until 09:01Z):

- **Coordinator Worker**, the first arena build on it. Over 7 h, 2,141
  deliveries were applied in `primary` mode (no RPC fallback round), with
  the liveness verdict `ok` throughout. With growable memory the stateless
  Worker held at most 4.2 MB of linear memory and the ChainJournal 13.3 MB
  (its parsed runtime); a fixed build would hold 96 MiB from instantiation.
  Route peaks: `journal/fetch` 30 pages, `worker/scheduled` 5, the other
  Worker routes at most 11. No allocation failure, glue spill, overdraft,
  quarantine, abandoned scope, ticket refusal, recycle or postmortem.
  ChainJournal constructions kept the pre-deploy rate (about two a minute,
  the idle eviction measured above). The epoch-61 commit (made by the
  previous build) was revealed on the new build with `weights_match`. From
  epoch 62 the installed bundle's authorization lease (epochs 53 to 61) has
  ended, so signing defers with `authorization_refused` until a renewed
  bundle is installed, and no submission step ran on this build.
- **Unauthenticated `/hook`.** A request with no signature header, and a
  chunked request with a well-shaped header for another subscription whose
  body stalled, were each answered 401 in under 0.2 s while the body was
  still open.
- **Load rehearsal `load-arenafu-r0p128-n1000`** (load instance, R_0 + 128
  synthetic copies at the load roster cap, n = 1,000 from a sealed batch, no
  fresh stock): 129,000 receipts, packaged and closed in 21 minutes with no
  step error. Peaks: `cohort/load-seal` 39 pages, `cohort/package` 1,281
  pages (80.1 MiB; the fitted step footprint is 1,336), `cohort/alarm` 8.
  The isolate's growable memory reached 96.9 MB (pool high-water 1,455 of
  1,512 pages) and the instance did not restart during the run: no
  allocation failure, glue spill, observed refusal or postmortem; one alarm
  scope quarantined 3 pages (below the quarantine budget; healthy).
- **Cohort 12 `testnet-rv1-f`** (main instance, reward-v1, r0-flat and one
  entrant, n = 1,000, the exposed pool `testnet-human-pool-pc1`): opened at
  epoch 63, drew, generated, scored 1,000 receipts and packaged at epoch 64,
  closed at epoch 67, all with no step error. `reward_v1_verify` verified
  the package (`NoTarget`, 1,000 receipts) and the closure offline under the
  published trust roots. Peaks: `cohort/draw` 222 pages (no derived
  footprint yet: 185 pages of overdraft, counted), `cohort/seal` 419 pages
  (2,224 tasks: batch, probes and audits; the fitted footprint is 430, the
  closest margin measured), `cohort/package` 12 pages at J = 2,
  `cohort/close` 2. The instance's memory reached 35.4 MB; no restart,
  allocation failure, glue spill or postmortem; one alarm scope quarantined
  1 page (healthy).

### Scorer go-live (2026-11-02)

eval-testnet runs the generalized scorer: `a-scorer` `d1224770...`, built
from main in the pinned rust:1.99.1-slim-bookworm image, with ORT
`fc44112f...` (image `90b18401...`). It admits a load by the artifact spec the
load names and by spec v2 when it names none. The pin is tracked in
`wrangler.toml` (`17be0399`), so later deploys from main keep it. The
coordinator Worker was not redeployed: it already ran main's coordinator,
protocol and arena trees (`3f9848fd`), which include the lane-client fix.

- **Deploy.** Both instances were idle (cohort 12 closed at epoch 67, nothing
  queued). The deploy ran from a clean commit on main, with the binary pin
  and the ledger value passed at deploy time. The Container application
  took a new version. The first probe, four minutes later, scored on the new
  image. `/healthz` reports memory model v3, specs v2 and v3, unnamed loads
  admitted by v2, and a `memory_limit` of 12,808,597,504 bytes on
  standard-4. The tower class's four-session bound is 12,078,473,215 bytes,
  so a tower load fits the 507 guard.
- **Rehearsal artifacts.** The reward-v1 rehearsal set was re-encoded for
  spec v2. Values are unchanged; the JSON is in canonical layout and each
  export manifest is bound to the variant's own files. `no-load` is now the
  adversarial `nonfinite-outputs` graph. The new parts and parts manifests
  are in the private eval bucket beside the old ones, which the signed
  packages of cohorts 6 to 12 name. Locally, with the deployed binary, every
  re-encoded entrant gave the same `status` and `output_sha256` as the
  previous scorer on the previous parts, document for document.
- **Probes** (operator scorer probe, two synthetic texts each, a fresh
  instance per package):

  | Package | Outcome |
  | --- | --- |
  | R_0 `2f88b0d2...` | 2/2 `ok`; load 22-45 s |
  | re-encoded `r0-flat`, `flat-near`, `h-near`, `ha-0.5`, `ha-2.0`, `soup` | 2/2 `ok` each; load 21-44 s |
  | `fail-all` | 2/2 `error` |
  | `no-load` | 502 `artifact load failed` |
  | old `r0-flat` `ce4fd4fd...` | 502 `refused profile_id` |
  | S2c `7013dfab...` | 502 `refused graph_size` (the probe's load names no spec, so spec v2 admits it) |

  Switching packages never returned a reused-instance 502. One `soup` probe
  failed `part upload` (an infrastructure code, not counted against the
  artifact), and the retry scored. Probe outputs equal the local scorer's
  bit for bit on some hosts and differ by up to 2.3e-5 on others, as before.
- **Load drill `load-golive-r0p12-n1000`** (load instance; rehearsal 2's
  sealed n = 1,000 batch, no fresh stock; W = 1, no prestart; 13 jobs):
  packaged and closed in 9 minutes, 13,000 receipts, no step error and no
  scorer failure. Every job had its intended outcome. The R_0 copy tied, with
  a utility difference of 7.8e-8 from host differences. The six re-encoded
  entrants scored 1,000/1,000. `fail-all` gave 1,000 model errors. `no-load`
  failed its load on three fresh instances, then its documents became model
  errors. The old `no-load` and old `r0-flat` were each refused once and
  definitively (`refused tokenizer_mismatch`, `refused profile_id`). S2c was
  refused before any load, `artifact over the declared size bound`: the
  plan's `artifact_max_mib` is 640, and the S2c object is 666 MiB. Six lanes'
  first starts did not answer within `SCORER_START_WAIT_MS`. Those lanes were
  restarted, and none of those starts counted against an artifact.
  `eval_verify` and the native validator's `package::verify` verified the
  package under the load instance's keys.
- **Not run.** No main cohort has run on this scorer. The main instance's
  reference chain names the earlier rehearsal reference, and this scorer
  refuses it (`refused profile_id`). So main runs no reward-v1 cohort until
  its chain record is restarted. A tower-class artifact is refused in every
  cohort until the coordinator's loads name spec v3 (`spec=` in
  `eval/scorer_client.rs` `load_path`, and the probe route) and the cohort
  template's `artifact_max_mib` admits the tower object. Until then, the S2c
  package's spec v3 admission, its document parity with R_0 and its span
  outputs rest on local runs of the deployed binary.
- **Rollback.** Wait until both instances are idle. Then redeploy with the
  previous binary (`9efb681a...`) staged and passed as the pin, and revert
  `17be0399` on main. The lane client works with either scorer.

### Coordinator admission review fixes (2026-11-02)

Branch `subnet-testnet/admission`, on main as `959bf738` (fixes) over
`d92a4019` (admission). The review's 39 findings, by outcome:

| Findings | Outcome |
| --- | --- |
| 1 zero header length loops the reader and the framer | fixed: the protocol reader refuses it; the framer parses nothing itself and counts its attempts |
| 2, 29 one undecodable commitment aborts every reader round | fixed: decode failures, empty fields, batch extrinsics, discarded state and a runtime change at the block end only that commitment |
| 3, 21 a lost multipart completion holds the freeze | fixed: the completion is a resumed step; only R2's definitive part refusal is `object_mismatch`; `/eval/session/refuse` and abandoning a faulted admission cohort are the operator's ways out |
| 4, 22 the calendar names a stale reference after a promotion | fixed: references follow a pending promotion, then `reference_state`, then the template; reinstalls never reset the chain; a seal mismatch faults |
| 5 two hotkeys at one position stop all work | fixed: shared extrinsics are refused; a refused frozen plan faults the cohort |
| 6 rule (b) bypassed by another claim | fixed: verdicts record the build they were reached on |
| 7, 15, 25, 26 classification and probe-route limits | fixed in classification (reply body, exact codes, control document); the dedicated admission route with the cohort's limits is a lane-client change (open) |
| 8, 28 discarded state retried forever | fixed: unresolvable, listing restart, snapshot fault |
| 9, 28 reader starves the RPC budget | fixed: the reader's own share |
| 10, 34 unauthenticated load and public exposure | fixed in part: routes off by default, `hotkeys` mode for drills, stateless signature check, one session per commitment, truncated hotkey view; rate limits open |
| 11, 27 fresh stock unlocked by non-working or operator entrants | fixed: published copies and failed control documents never unlock; a fresh pool needs the operator list; chain-owner (coldkey) checks open |
| 12, 31 `SpendLedger` names | admission runs settle; the `SpendLedger` rename is a lane-client change (open); deploys pass the old name |
| 13, 23 spec v3 cohorts wait forever | fixed: admission plans are spec v2 only until runs name the spec |
| 14 platform retries forever | fixed: doubling backoff, status counters, operator refusal |
| 16, 35 framing and reader memory | fixed in part (one parse, file-count bound, one large read a round, one copy); measurements open |
| 17 reader body bound | fixed (one large read a round) |
| 18 contract details | fixed: transient not-found never refuses, a retried creation answers first, admission arena refusals fault the admitting cohort, runs count scoring lanes |
| 19, 38 `float_roundtrip` | declared in the coordinator manifest as evaluation semantics; `BILLING_CYCLE_DAY` reworded |
| 20 checks not rerun | rerun (see below) |
| 24 header decoding in the key-holding Worker | open: needs a lane-client change or a key-free Worker; routes stay off outside drills |
| 30 blobs of non-admitted uploads | fixed: the blobs and manifest a session created are deleted with it |
| 32 template vs the validator bundle bound | open (bundle schema v3, W7) |
| 33 freeze not at a tempo boundary | fixed |
| 36 cron-driven reader | open (needs a ruling) |
| 37 W = 1, no pre-start, incentive | fixed in the template check; the D12b canary rule is open |
| 39 confidentiality and gates | no change needed; the trust file pins no admission origin until the public-access decision |

Checks (build-host, one Cargo job, `-j 4`, offline, `--locked`): coordinator
lib 229/229, clippy native and wasm32 `-D warnings` clean, protocol 94,
miner and wire 42, `no_runtime_gates` 2/2, publisher 50, subnet workspace
clippy `-D warnings` clean, `cargo metadata --locked`.

### Coordinator admission, live drill (2026-11-02)

The eval Worker runs main `959bf738` (store v9 to v11 in place, migrated on
the first request; Container application unchanged). The deploy opened the
admission routes in drill mode only: `ADMISSION_ROUTES=hotkeys`, with
`ADMISSION_HOTKEYS` naming the three operator-held test-miner hotkeys,
passed at deploy time (the tracked default stays `off`). A signed request
from any other hotkey gets 404 at the Worker; the unsigned cohort read
answers. The test-miner wallets were registered on 601 by burn (UIDs 4 to
6); a shielded registration sent within a few blocks of another failed as
Invalid and went through on its own retry.

- **Template and calendar.** A standing template (`testnet-adm`, n = 200,
  K = 4, one admission tempo, a roster of at most 4, spec v2, exposed pool, the test
  miners listed as operator hotkeys, no fresh pool, W = 1, no pre-start, the
  incentive rule with provisional gates) was installed with
  `POST /eval/template`. The calendar queued cohort 13 `testnet-adm-13` at
  once (cohort 12 closed) and the engine opened it at the next boundary
  (epoch 71). The install reply named the next references from the chain
  (`source: chain`): the main chain's R_k is the old `r0-flat`, which the
  generalized scorer refuses, so this cohort cannot package (see below).
- **Two fresh hotkeys admitted.** Test miners 1 and 2 packed re-encoded
  rehearsal artifacts (`ha-0.5`, `ha-2.0`) with `slop_ninja_miner pack`,
  committed them (`commit`) and uploaded them (`upload`, about 613 MB each,
  64 MiB parts). Each first session request answered `commitment_not_found`
  (the log had not resolved the commitment yet) and the client's retry
  opened the session. Completion, framing, the blob copies and the verdict
  run on a fresh scorer instance each finished within minutes; both
  sessions were `verified`, then `admitted` at the freeze, ranked by
  position (8357097-6, then 8357106-5).
- **Overwritten commitment.** Test miner 3 committed, uploaded and was
  verified (8357116-5), then committed again for the same cohort
  (`commit --replace`, 8357171-5) without uploading. At the freeze its
  session was refused `commitment_overwritten`; the snapshot at
  `cutoff_block - 1` had the newer commitment.
- **Cutoff and freeze.** The window closed at the epoch-72 boundary (cutoff
  block 8357439). The snapshot was read within the same epoch and the
  roster froze at that boundary: 2 admitted, 0 overflow, 1 refused; no
  outside entrant (operator hotkeys), so the cohort draws exposed stock.
  The commitment log used its own RPC share throughout (about 1.5 reads a
  minute while admitting).
- **Client note.** For test miner 3 the miner CLI reported "no commitment in
  finalized state after inclusion" although the commitment had landed and
  the coordinator logged it at its position; `status` showed it, and
  `upload` used the matching opening. The client reads `CommitmentOf` at the
  finalized head once, right after finalized inclusion; a read that lands on
  an endpoint whose finalized head is still behind sees nothing.
- **After the freeze.** Cohort 13 drew exposed stock, generated, sealed
  (binding the two admitted entrants with their own openings and
  positions) and scored both entrants. Its reference, the main chain's old
  `r0-flat`, is refused by the generalized scorer (`refused profile_id`); a
  refused reference counts as a scorer failure, so the cohort faults
  `scorer_unavailable` only after the plan's `failures_before_pause` and is
  then abandoned. The template is paused, so no further cohort opens on the
  old chain.
- **Not run here.** A coordinator-opened cohort opening at its
  predecessor's close: no cohort on main's current chain can reach its
  transition, so this waits for the chain-record restart (cohort 13 itself
  was queued and opened by the calendar, with no operator plan, at the
  first boundary after the template's install; the queue during a
  predecessor's transition is covered by tests).
- **Routes after the drill.** Redeployed with `ADMISSION_ROUTES` off once
  both instances were idle.

### Chain-record restart

A restart gives the main instance a fresh reference chain (R_0, no
predecessor) without resetting or deleting anything: the eval Worker names a
new main authority (README "Main authority"). The earlier authority's store,
objects, packages and receipts stay where they are, readable at `GET
/eval/authority/<name>/status`; once the new authority records succession it
admits no plan again.

Deploy rules for every step that deploys, in addition to the adopted ones
(one deploy window under `data/locks/cf-deploy.lock`, a clean pushed commit
that contains the deployed one and the scorer pin, the scorer build context
staged by the tracked pin, one change per deploy):

- The authority vars (`EVAL_MAIN_AUTHORITY`, `EVAL_MAIN_FIRST_COHORT`,
  `EVAL_MAIN_PREVIOUS`) and the publisher's record vars
  (`RECORD_LABEL_PREFIX`, `PUBLISH_FROM_EPOCH`) live only in the tracked
  `wrangler.toml` files. Never pass them with `--var`: a deploy from any tree
  then carries the authority and record that tree names. Refuse a deploy
  whose command line names any of them (`--var EVAL_MAIN_...`, `--var
  RECORD_LABEL_PREFIX:...`, `--var PUBLISH_FROM_EPOCH:...`, or a `--vars`
  file holding them); a deploy script written before these vars were tracked
  (one that passes the record vars of an earlier deploy) is never reused.
  The pre-deploy checks below read only the tree, so they cannot see a
  `--var` override; a publisher deployed with a stale prefix publishes
  nothing and raises `publication_refused` until it is redeployed.
- Before every eval deploy, inside the deploy window, read `GET /eval/status`
  and run `slop_ninja_eval_ops authority-check --wrangler
  subnet/coordinator/wrangler.toml --status <it>`; refuse the deploy unless it
  passes (exit 0). The check fails closed: an error reply, a Worker status
  whose `eval_cohort` is null, or a status without `authority_name`,
  `first_cohort`, `predecessors`, `cohorts`, `superseded_by`,
  `succession_recorded` or `work` refuses. It reads succession from what the
  stores recorded, never from the cohorts listed: a routine deploy over an
  authority a successor superseded is refused (it admits nothing; `--freeze`
  alone redeploys it frozen, deliberately). Only step 1's deploy uses
  `--first-deploy`, and only the switch (and a rollback) commit's deploy
  uses `--switch`; a rollback also passes `--target-status` (below).
- Before every publisher deploy, read the live index (`v1/cohorts/index.json`
  in the public-candidate bucket) and run `slop_ninja_publisher_ops
  record-check --wrangler subnet/publisher/wrangler.toml --index <it>`
  (publisher crate example); refuse the deploy unless it passes. It refuses a
  tree whose `RECORD_LABEL_PREFIX` is not the live record's, an earlier
  record's prefix above all. Only step 8's commit uses `--restart`.
- Tell every lane that deploys eval-testnet or the publisher when the switch
  commit lands on main: their trees must contain it.

1. **The code, unchanged behaviour.** Deploy the eval Worker from a commit
   with authority support (the tracked `EVAL_MAIN_AUTHORITY = "main"`, empty
   first cohort and predecessors), after `authority-check --first-deploy`
   passes: the deployed build reports no authority, and the tree tracks the
   original. From then on `/eval/status` reports `authority_name: "main"`,
   `authority_retired: false`, `superseded_by: null` and the same cohorts as
   before, and every later deploy runs the check without that flag. Deploy
   the coordinator Worker the same way: `pause` signing, deploy, wait until
   `/status` reports journal schema version 8 (read only), then `resume`.
   Never call the restart route to probe the version: it is an admin action
   (a refusal changes nothing, but a valid bundle restarts the held target).
2. **Quiesce the original.** Pause its calendar if a standing template is
   installed (`POST /eval/template/pause`), then wait until every cohort on
   both instances is closed or abandoned and none is queued (the go-live idle
   check), and until `/eval/status` reports `work: false` (the last cohort's
   closure, settlement and purge done). The switch check (step 4) refuses an
   authority that is not idle, has work left or whose template is not
   paused, and the original records a successor only while idle with no work
   left (step 5).
3. **Restart record.** Save the main status (`GET /eval/status`), then:

   ```text
   slop_ninja_eval_ops restart --authority <name> --status main-status.json \
       --wrangler subnet/coordinator/wrangler.toml --reference r0-artifact.json \
       --expect-package <R_0 package digest> --probe-template <a probe plan> --out <dir> \
       --decision <the recorded decision>
   ```

   `<name>` is lowercase letters and digits. The tool refuses a status whose
   authority (name, first cohort, predecessors) is not the one the tree
   tracks, an authority that is not idle, has work left or is already
   superseded, a missing `--decision` (the recorded user decision the
   restart runs under, kept in `restart.json`), a name already used, a name whose
   label prefix `testnet-<name>-` an earlier label has, a reference record
   whose `package_sha256` is not `--expect-package` (the R_0 package the
   restart was decided on, so a stale record never starts the chain), and a
   plan the coordinator would refuse (at the tracked `ROSTER_CAP`).
   `r0-artifact.json` is R_0's artifact record as plans name it (owner,
   contract, package and model digests, the scorer image digest, commitment
   block, manifest key). `<dir>/restart.json` holds the three authority vars
   to commit (`EVAL_MAIN_FIRST_COHORT` is one past the replaced authority's
   highest cohort number; `EVAL_MAIN_PREVIOUS` lists the earlier named
   authorities), the publisher's `RECORD_LABEL_PREFIX` and
   `PUBLISH_FROM_EPOCH`, `record_from_epoch` (the same epoch), R_0 and the
   probe record's key; `<dir>/probe-plan.json` is the probe build.
4. **Switch.** Commit exactly the three vars from `restart.json` in
   `[env.eval-testnet.vars]` (a one-line-per-var commit, nothing else), push
   it to main, and deploy eval-testnet from it after `authority-check
   --switch` passes. It passes only when the tree's predecessors are the
   deployed authority's and then the deployed one, the deployed authority
   holds no cohort that is not closed or abandoned and reports `work: false`
   (no closure, spend settlement, purge of private results and summaries, or
   admission clean-up left), its standing template is paused (or none is
   installed), none of its labels or numbers falls within the new
   authority's (every number below `EVAL_MAIN_FIRST_COHORT`), and it
   recorded no other successor.
   Then wait for the rollout before step 5: at least 5 minutes after the
   deploy completes (Durable Objects may run the previous code for up to
   300 s, and a lane object on old code ignores `LaneKick.authority` and
   reports to the original, which costs liveness, not safety), and until
   `/eval/status` reports `authority_name: "<name>"`, `predecessors: ["main",
   ...]`, no cohorts, `first_cohort`, `label_prefix` and `reward.references`
   null. `/eval/authority/main/status` reports the original's cohorts
   unchanged and `authority_retired: true`.
5. **Chain record.** `POST /eval/plan` with `probe-plan.json`. The named
   authority first has every predecessor record it as successor, in order. A
   409 names why not: `predecessor_busy` (a predecessor still holds an open
   cohort; it is kicked on ticks and finishes, so post again once it is
   closed or abandoned), `predecessor_work` (its cohorts are closed, but a
   closure, spend settlement, purge or admission clean-up is left: the ticks
   keep kicking it until succession completes, so post again once its
   status reports `work: false`; work left after succession would never
   run), `predecessor_numbers` (a first cohort or prefix that
   collides), `predecessor_superseded_elsewhere` (a predecessor recorded a
   successor that is neither this authority nor one listed after it in
   `EVAL_MAIN_PREVIOUS`: the deployment left an authority out, so fix the
   list and redeploy), `predecessor_unknown` or `predecessor_unreadable`. A
   superseded authority queues nothing, also for a plan admitted before its
   successor recorded it. Afterwards `/eval/authority/main/status` reports
   `superseded_by: "<name>"`, the original admits nothing even under a
   reverted deploy, and ticks stop kicking the predecessors (they hold
   nothing, have no work left and take nothing). `/eval/status` reports
   `succession_recorded` (each predecessor with the first cohort it was
   checked for) and `succession_complete: true`. Succession is recorded
   before the plan queues, so a refused or failed queue leaves it recorded
   while the named authority still lists no cohort. The probe build draws, generates and scores
   R_0 on its probe documents and closes with `reward.references` `{k: 0,
   current_content: <R_0 content>, predecessor_content: null}`, R_0
   published and fingerprinted, the probe record at
   `probes/authority/<name>/v1/record.json` in the eval bucket, and
   `launch_burn: true`. Read that record back privately, then build each
   rehearsal cohort with

   ```text
   slop_ninja_eval_ops restart-cohort --restart <dir>/restart.json --record record.json \
       --template <a v3 plan> --cohort <n> --label-suffix <s> --wrangler subnet/coordinator/wrangler.toml
   ```

   (R_0, no predecessor, the authority's probe version, label
   `testnet-<name>-<s>`, a named artifact spec's digest frozen with the plan
   in `fanout.score.artifact_spec_sha256`). A standing template's
   `label_prefix` is `testnet-<name>`; its install needs the same succession,
   and its calendar numbers from the first cohort.
6. **Launch bundle and the coordinator.** With `F` the restart record's
   `record_from_epoch` and `E` the current epoch (`E >= F`):

   ```text
   reward_v1_verify launch --trust testnet-trust.json --authorization-seed <seed file> \
       --issued-epoch E --out launch-<name>-eE.json
   ```

   On the coordinator, with the operator token: `{"action": "pause"}` on
   `POST /signing/control`, then `POST /signing/restart?from_epoch=F` with
   the bundle file `launch-<name>-eE.json` as the request body, then
   `{"action": "resume"}` on `/signing/control`. The control route takes
   only small JSON actions and refuses `restart_target` there. The restart
   route answers 200 with `{"restarted": ...}` (the record set aside, the
   target it replaced, the launch target now installed, `F`) or
   `{"refused": {"code": ...}}` with nothing changed: `signing_active` (not
   paused), `restart_regressed` (`F` before an earlier restart's, or not
   after the latest epoch an installed bundle was issued or opened at,
   `record_epoch` in `/status`, the earlier restarts' own launch bundles
   included),
   `restart_before_held_record` (`F` at or before the epoch the held target
   activated), the bundle's own refusals (`bundle_malformed` and the other
   verification codes), `not_a_launch_bundle`, `authorization_refused`, and
   `bundle_before_restarted_record` (an authorization issued before `F`).
   Before the journal, it answers 400 `from_epoch` (the parameter is missing
   or not a number), 400 `bad_request` (a body that is not UTF-8), 413
   `body_too_large` (a body over the bundle bound) and 401 without the
   token. The next commit is the launch burn row; earlier commits keep their
   read-back and reveal checks, no epoch takes a second operation, and no
   bundle issued or opened before `F` installs again (`restarted_from_epoch`
   in `/status`), nor any T12 bundle (`v1_after_restart`).
7. **validator2 and the authorization.** No component writes the private
   authorization pointer yet (`authorizations/current.json` in the receipts
   bucket; publisher README, "Not yet done"), so outside validators see no
   restarted authorization until that follow-up lands. validator2 holds the
   earlier chain's target under the same trust roots, so install by hand
   (validator README, "Trust roots and restarted chain records"): stop the
   dispatch unit, run `reset-target`, install the launch bundle file from
   step 6, start the unit. Rotated roots would instead set the record aside
   on their own (`held_target_authority_changed`).
8. **Publisher.** Commit `RECORD_LABEL_PREFIX` and `PUBLISH_FROM_EPOCH` from
   `restart.json` in the publisher's `[env.testnet.vars]` and deploy it from
   that commit after `record-check --restart` passes. Its next run sees the
   listed cohorts outside the new prefix, archives the previous record's
   index at `v1/records/<its first label>/cohorts/index.json` (still in the
   private public-candidate bucket; written once and never replaced, and
   refused if it carries wording public text never carries), removes the
   live leaderboard and reference pointers (so the earlier wording they
   carried is gone, never archived), and starts a fresh index that records
   the earlier record (`previous_records`, with its initial references, which
   stay withheld as entrant artifacts). The record starts at the first
   restarted cohort whose reference is R_0 with no predecessor; the earlier
   record's cohorts are never appended to it, and a later deploy that names
   the earlier prefix again is refused by the publisher itself (the run
   publishes nothing and raises `publication_refused`). The earlier record's
   cohorts whose detailed receipts were still published lose them once two
   cohorts of the new record are published, whether or not those produce a
   target (the restart superseded the earlier target; their publication
   records say when). Read back `v1/records/<first label>/` after the run:
   it holds only `cohorts/index.json`.

Rollback. Only before any succession was recorded: the earlier authority
recorded no successor (its `GET /eval/authority/<name>/status` reports
`superseded_by: null`), the named authority recorded none
(`succession_recorded: []`) and lists no cohort, and its standing template is
paused or absent, so no succession starts between the check and the deploy.
Listing no cohort is not enough: succession is recorded before a plan queues
and before a template's first cohort opens. Commit the three vars back, save
both statuses, and deploy with `authority-check --switch --status <named
authority's /eval/status> --target-status <the earlier authority's
/eval/authority/<name>/status>`; then the original is current again, exactly
as it was. Once succession was recorded on either side, `--switch` refuses
the rollback: the earlier authority is superseded and admits nothing, and a
deploy that names it again only leaves the named authority retired, its
cohorts paused until the switch commit is deployed again (never skipped), so
nothing admits anywhere. Only a deliberate freeze passes, with `--switch
--freeze`, and later deploys of that frozen tree need `--freeze` too. To
abandon a restart before the announcement, restart again under a new name.
The coordinator's restart is not rolled back either: the earlier record's
bundles never install after it; a further restart installs a newer launch
bundle at a later `F`.

Second restart (before the announcement only): repeat the steps with the
current authority's status. The tool adds the replaced named authority to
`EVAL_MAIN_PREVIOUS`; the new authority has every predecessor (the original
and each named one) record succession, and the predecessors are kicked on
ticks until it has.

After the announcement. A restart installs a fresh R_0 with no predecessor,
which outside miners would see as a reset of the reference chain they
entered. Decision U2 allows exactly one change of reference after the
announcement: R0 replaced once, by the round-2 reference, after it passes
every gate, announced in advance on the status page, as a `k = 1` successor
of R_0 on the same chain record (a plan with a predecessor), never as a
restart. So no restart runs after the announcement without a new recorded
decision from the user, and `restart` requires `--decision <reference>`
naming the decision record it runs under; the restart record keeps it.

Artifact specs: a plan that names `fanout.score.artifact_spec` names its
digest too (`fanout.score.artifact_spec_sha256`, or the admission block's),
equal to this build's; every scoring load names that frozen digest, and the
operator probe takes the same field (`POST /eval/scorer-probe` with
`"artifact_spec": "slop_ninja_a_artifact_spec_v3"`), so a tower-class package
is admitted by spec v3. The probe also takes `"spans": true`, which asks the
scorer for each probe document's sentences and spans (a package with a token
head). The spec's bounds size the artifact. A plan that names none is
admitted by spec v2, as before. A plan that carries
`fanout.score.artifact_spec_sha256` binds limits record v2 (the spec by id
and digest, published in `components.json`); every other plan, one that
names its spec only in the admission block included, binds limits record
v1, so a stored plan's record never changes with the build. Admission runs
(the admission lane's `/run`) still load by spec v2 only.

### Chain-record restart, live (2026-11-02)

The main instance's reference chain was restarted once, before the
announcement, under the named authority `r0`, with the path-stripped R0
package as R_0 and no predecessor (`restart --decision` names the recorded
decisions: strip R0's provenance paths, and publish the rewritten
`fit_binding.contract_sha256` with the rewrite record beside the package).
Nothing of the original authority was reset or deleted.

- **Deploys**, each in its own deploy window from a clean main commit that
  contained the deployed one and the scorer pin, with the scorer build
  context staged by the tracked pin:
  1. the coordinator from `bbeed582`: signing paused, journal schema 7 to 8
     read in `/status`, signing resumed;
  2. eval-testnet from `bbeed582` after `authority-check --first-deploy`:
     store schema 11 to 12, `authority_name: "main"`, `work: false`; the
     scorer image was unchanged;
  3. the switch commit `04d01160` (`EVAL_MAIN_AUTHORITY = "r0"`,
     `EVAL_MAIN_FIRST_COHORT = "14"`) after `authority-check --switch`;
     after the 5-minute rollout `/eval/status` named `r0` with predecessors
     `["main"]`, no cohorts and label prefix `testnet-r0-`, and the original
     reported `authority_retired: true`;
  4. the publisher commit `72c311cd` (`PUBLISH_FROM_EPOCH = "76"`,
     `RECORD_LABEL_PREFIX = "testnet-r0-"`) after `record-check --restart`.
  No record var was passed with `--var`. The platform truncates a long
  deployment message (one eval deploy's reads `... chain-restart-live
  bbee...`; its commit is `bbeed582`), so keep messages short enough to show
  the commit, which the deploy rule reads.
- **R_0.** Package `143ca2d9…` (content `d7232991…`), the spec v2 pack of the
  stripped files, was uploaded to the private eval bucket with its
  provenance rewrite record. Operator probes, unnamed and naming spec v3,
  scored both texts: equal to the local scorer bit for bit on one Container
  host class, and within 2.3e-5 on the other (where they equal the earlier
  live probe of the unstripped R_0 bit for bit).
- **Restart record.** `restart --authority r0 --expect-package 143ca2d9…`:
  first cohort 14, label prefix `testnet-r0-`, record epoch F = 76.
- **Chain record.** `POST /eval/plan` with the probe build recorded main's
  succession (`superseded_by: "r0"`; `succession_recorded` `[{main, 14}]`)
  and queued it. The probe build `testnet-r0-probe-v1` (cohort 14, exposed
  pool, 1,024 documents) opened, packaged and closed within epoch 77, with
  `reward.references` `{k: 0, current_content: d7232991…,
  predecessor_content: null}` and the launch burn; its record
  `probes/authority/r0/v1/record.json` was read back privately.
- **Coordinator.** `POST /signing/restart?from_epoch=76` with a launch
  bundle issued at epoch 77 (lease to epoch 117) replaced the target the
  coordinator held until then; signing was paused around it. The coordinator commits the burn row
  `[[0, 65535]]` each tempo from epoch 77; epoch 77's reveal read back with
  `weights_match`.
- **validator2.** Stopped, `reset-target`, the launch bundle installed, and
  started (validator README, "Trust roots and restarted chain records"). It
  verifies the launch target and defers each tempo with
  `row_would_be_held`: the burn row pays only the owner hotkey's UID, which
  rtao `hold_previous` holds at the pinned revision. So under the launch
  burn only the coordinator commits; validator2 commits again once a target
  pays a registered miner or the burn-row opt-in lands.
- **Publisher.** Its first run archived the earlier record's index at
  `v1/records/testnet-rv1-d/cohorts/index.json` (the only object there),
  removed the live leaderboard and reference pointer, and started a fresh
  index for `testnet-r0-` that lists the earlier record under
  `previous_records`.
- **Reference packages** (public-candidate bucket, public access off): the
  directories of R_0 `143ca2d9…` and the S2c span package `daff49c2…`, each
  with its `provenance-rewrite.json`, and `packages/index.json`, staged from
  one tree that the package-corpus scan, the byte-level quoted and bare id
  passes and a home-path check passed on the exact bytes; every object read
  back equal to the scan's sums. The unstripped R_0 directory `2f88b0d2…`
  was then deleted and read back absent.
- **S2c.** An operator probe naming spec v3 with `"spans": true` scored both
  texts; on the bit-exact host class its predictions, sentences and spans
  equal the local scorer's. The probe reply carries no measured memory peak
  (the tower class's four-session bound, 12.08 GB, is within the instance's
  12.81 GB limit), and its host capture reads `cpu_model` from the scorer's
  `/info`, which reports `cpu.model_name`, so probe replies name no host.
  Cohorts still name spec v2, so S2c is probe-qualified, not yet scored in a
  cohort.
- **Rehearsal cohorts** `testnet-r0-a`, `-b` and `-c` (15 to 17) were built
  with `restart-cohort` from the standing template's plan without its
  admission block (n = 200, K = 4, spec v2 named, exposed pool, no entrants,
  W = 1, no pre-start, 64 lanes) and queued back to back. Cohort 15 opened at
  epoch 78 and packaged at epoch 79: R_0 scored the 200 cases (one job, no
  scorer failure), the outcome is `NoTarget`, so the launch burn continues,
  and `reward_v1_verify cohort` verified the signed manifest, package and
  all 200 receipts from the receipts bucket. It closed at epoch 82, and its
  closure verified the same way. The publisher then published it: the
  index lists it, and the reference pointer and leaderboard name R_0 by
  content (`d7232991…`) and package (`143ca2d9…`), linked to the staged
  directory under CC BY-SA 4.0, with nothing withheld. Cohort 16 opened at
  epoch 82, back to back, and packaged at epoch 83 (`NoTarget`, 200
  receipts, verified the same way); cohort 17 is queued behind it.
- **Chain.** The coordinator's burn rows for epochs 77 to 81 were each
  revealed with `weights_match`, and its on-chain row is `[[0, 65535]]`.
  validator2's on-chain row is still the earlier record's `[[3, 65535]]`,
  which pays one of our own test UIDs; it is replaced only when validator2
  can commit again.

### Cost projection (daily K = 20 cohorts, n = 1,000)

The projection's inputs come from the load rehearsals of 2026-10-31 (3 to 129
jobs): each scoring lane paid one cold path (about 30 s and 24 vCPU-s), and
documents took 1.15 to 1.18 vCPU-s each; a lost chunk's re-run adds one cold
path on the lane that takes it. The cost figures themselves are kept out of
this runbook.

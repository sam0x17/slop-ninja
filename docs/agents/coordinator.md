# Coordinator and validator rules for contributors

[`AGENTS.md`](../../AGENTS.md) summarizes these rules and links here for the
full set. Whitepaper Sections 5 and 6 and the
[Cloudflare design](../cloudflare-subnet-draft.md) are the canonical
statements. Keep this page, the whitepaper and the design notes aligned when a
rule changes.

## Score package, public result and row record

The coordinator alone measures entrants, computes
the reward outcome and derives the native row. It signs a private score package
(exact scores, every hidden measurement, the quantization draws, the receipt
root and count), which never leaves its storage, and a public quantized result
(per entrant its quantized score or withheld, band, gate and public-copy bits,
cluster membership and units; the references' quantized scores; the bit
budget's terms), from which anyone can recompute the row. Once a tempo, from its
own finalized registration snapshot and independently of its own commitment, it
derives the active target's row and signs a new coordinator row record when the
paid entries change (chain binding, reward rule, cohort and manifest digest,
target and authorization, epochs, derivation block, owner hotkey, and per paid
UID its hotkey, registration block and integer weight). The record is published
off-chain beside the authorization and the signed revocation set as soon as it
is signed, before the coordinator's own hotkey signs anything; no validator
waits for the coordinator's commitment.

Every other validator
verifies the record under its pinned keys, checks that it names the authorization
and a held or later target, journals the revocations, and commits its entries
through its own journaled commit-reveal; its only change burns the units of a
registration its own finalized snapshot no longer holds through the owner
hotkey's UID, deferring the tempo when that hotkey holds no UID. It needs
nothing else to commit.

## Artifact intake and publication

A artifacts stay private until their cohort's scoring
starts. A miner hashes each file once when it packs, commits only a digest on
chain, and uploads the header and each file straight to the coordinator's private
R2 admission bucket with presigned PUTs; no upload passes through a SlopNinja
Worker. Each URL binds the derived key, length, committed SHA-256 and
create-only, so R2 verifies the bytes on arrival and stores nothing on a
mismatch; admission, publication and scoring trust R2's stored checksum and size
and never hash model bytes again. TLS and the private bucket are the privacy
boundary; there is no encryption layer, since R2 can verify only the bytes it
receives. Every admitted artifact is published when its cohort's scoring starts,
copied inside R2 or by a stream R2 verifies, and a published artifact is never
recalled. The quantized result's allow-listed values and the openings are
published when the cohort closes. No rank field is published: any order of
entrants follows from the published quantized scores and roster order alone.
Validators need none of them.

## Receipts and retention

Every inference attempt has a content-free R2 receipt bound into the coordinator's
signed score package: coordinator-private audit evidence, never published or sent
to validators. Outage probes are a named exception: a probe is one bounded
call on fixed control text, never cohort text, that tests whether the platform
an outage paused has recovered; it writes no receipt, and its runs, time and
spend are recorded with the cohort. Operator pre-flight probes and admission
runs of an entrant's artifact on the control document are named exceptions
too: they also run only on fixed control text, never feed a score, and write
no receipt. The coordinator installs a new target only when that cohort's
receipt index verifies.

Delete cohort N's detailed receipts and index after N+1
and N+2 complete and N's target is superseded, based on durable coordinator
completion rather than tempos, wall time, or adoption. Keep the evaluation copies
of R_k and R_{k-1} while they are references; delete admission-bucket copies of
entrant artifacts once their cohort closes and publication is recorded. Preserve
signed per-miner score/calculation records, roots/counts, rows, signed row
records, observation certificates, and chain/accounting recovery state.
Validators' commitments need only the signed row; submitted obligations recover
without expired receipts. No indefinite detailed receipt archive. The testnet
evaluation service runs this deletion; the receipts of the cohorts just before
the testnet record restart are not yet deleted (whitepaper Section 6).

## Cohort timing, activation and authorization

A cohort lasts at least K = 20 tempos of 360 blocks, about one day, frozen in the
manifest (rehearsals may use K = 4); overruns move to a later tempo boundary and
never skip. It ends with a transition of at least two tempos that issues no new
challenges. Cohort N's target activates at the first eligible tempo boundary
after its package and receipts are durable, and every validator commits the
coordinator's signed row for it every tempo with commit-reveal on, with no
per-validator activation gate
(when that row, or the launch row before any target, is the owner burn row,
native validators commit it under rtao's
`OwnerRowPolicy::Submit` when the chain limits allow it, and otherwise defer
that tempo; the coordinator commits it too).

Reward-intent authorization is per cohort and never expires: it stays valid
until it is superseded (another target activates) or a signed revocation
withdraws it, so validators keep committing the last signed row through
coordinator silence. Its renew-by hint (at least two cohorts) never pauses
anything. A validator journals each revocation before it takes effect; an entry
that no longer decodes or matches its digest is treated as a revocation still
in force (alerted, and applied from its recorded target and epoch; when its
target does not parse it withdraws every authorization issued at or before
that epoch) until the operator restores or removes it; signed commitments and
their reveals stay recoverable.

## Operator decisions

Never let our own rules lock the operator out (`AGENTS.md`). Whitepaper
Section 6 specifies these journaled decision paths:

- Abandon: the operator may end any cohort that has not closed, including a
  queued cohort or one with no fault. The cohort produces no target and its
  number is never reused. An abandonment in packaging or the transition, when
  the outcome may already be known, declines that outcome and needs the
  operator-decision key.
- Refusal to pay: the operator may refuse to pay a finished cohort by declining
  its closure, by abandoning it late, or by naming it in a switch to burn, each
  under the operator-decision key. A declined result is never installed;
  continuing after a refusal goes through a switch to burn and a fresh record.
- Start over: the operator switches the coordinator to burn. With signing
  paused, the coordinator sets a restart floor one epoch after the last
  finalized one, installs the burn row from the floor under a launch
  authorization issued at that epoch, and names in the signed restart record
  every cohort that opened before the floor without being handled. The operator
  then opens a fresh record whose first reference is R_0, with no predecessor.
  Winners of the earlier record are never paid again. Validators accept the
  restart through that launch authorization and refuse rows from before its
  floor; none needs a manual reset.
- Reward-rule change: a new reward rule takes effect only through a fresh
  record under that rule, reached by the same switch to burn.
- Reference decision: the operator may set the current and previous references
  directly, with a recorded reason, outside a fresh record.
- Re-registration: if the subnet is registered again on the same chain, the
  restart also rotates the authorization key; validators receive the new key
  through a signed release or trust update.

Each decision is journaled before it takes effect and leaves a signed,
content-free public record that states its reason. Decisions that decline a
result, change reward inputs or restart the record are also signed with a
separate operator-decision key, kept offline with a recorded backup. That key
is distinct from the coordinator's hotkey and its authorization and receipt
keys, and only the coordinator and the evaluation service verify it; validators
keep verifying rows, authorizations and revocations under the pinned
authorization key alone. Rotating the operator-decision key is a recorded
configuration change of those two services that needs no signature under the
old key, so losing it locks nothing. The paths remain available on mainnet with
the same signatures and public records. No outage triggers one, a decision
changes only future work and payment, commitments already submitted reveal
under their original rules, and rewards already paid are never reassigned.
Implemented so far: the record restart and the abandonment of a faulted cohort;
the other paths and the operator-decision key are not yet built.

## Block webhooks and the fallback watchdog

The rtao-rpc finalized-stream block webhook is the coordinator's primary
heartbeat and clock: it delivers every finalized block, and each delivery is a
separate Worker invocation. The webhooks are live as an unbilled Finney pilot
and a separate testnet service, each with its own subscriptions and signing
keys; receiver integration and qualification remain SlopNinja's work.
Authenticate each delivery and persist it before acknowledgment, deduplicate
it, and advance a finalized block height/hash cursor. Backfill any height gap
through bounded RPC, so no block is skipped. Notifications alone establish
neither finality nor authorization. Due work is claimed through durable leases
with idempotent operation IDs, so overlapping invocations skip it and later
blocks re-claim stale claims and continue resumable steps. A one-minute cron
watchdog is the only fallback and a no-op while deliveries arrive. If valid
deliveries stop for five minutes (configurable), or the cursor lags the
finalized head by that threshold, it polls the public OpenTensor RPC through
the same bounded per-block path, and returns to webhooks automatically when
they resume. Record mode transitions durably. The nominal wait is one
finalized block plus delivery latency, or up to the threshold plus a cron tick
in fallback; RPC/finality latency, catch-up, and outages can extend it.
Qualify measured lag before launch.

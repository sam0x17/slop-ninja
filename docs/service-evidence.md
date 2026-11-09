# Service evidence and recovery

The [whitepaper](../whitepaper/slop_ninja.pdf) and the
[Cloudflare design](cloudflare-subnet-draft.md) define the current protocol
proposal. Launch includes coordinator-scored *A*.
This guide identifies its evidence boundaries and the existing implementation;
it does not specify a deployed service.

One lead validator serves as the coordinator and is operated by SlopNinja.
The coordinator is the serverless service: it performs hidden evaluation,
scheduling, and all of its own validator duties on Workers and Durable Objects.
Its validator hotkey, authorization keys, and receipt keys have distinct roles
within that service. Its durable chain journal remains separate from public
evidence. Other validators run native Rust applications and receive no private
evaluation data.

## Evaluation records

The coordinator freezes the cohort's submissions, evaluation semantics,
workloads, labels, and scoring configuration before evaluation. Signed records
bind the admitted artifacts, input commitments, outputs, calculation, and
cohort identity. The coordinator alone derives the reward outcome and the
native row. It signs a private score package (exact scores, every hidden
measurement, the quantization draws, and the receipt root and count), which
never leaves its storage, and a public quantized result: the grid width, each
executed entrant's quantized score, band, gate and public-copy gate bits and
cluster, and the references' quantized scores. Fixed arithmetic, ordering,
normalization, and rounding over the quantized result yield the canonical row,
so anyone can recompute it as an advisory check; no chain runtime version is an
input. The coordinator signs a row record whenever the paid entries change and
publishes it off-chain before its own hotkey signs anything. Other validators
verify that record under pinned keys and commit it; they trust the hidden
evaluation and receive no documents, labels, per-example predictions, exact
scores, receipts, customer text, or private weights.

The recognized subnet owner sets `commit_reveal_weights_enabled = true` through
the agreed owner-authorized path under any lease. The operator verifies the
effective setting.
Validators check finalized state at startup and fresh submission state before
signing; an unexpectedly disabled flag pauses affected new commitments without a
silent direct-submission fallback. Validators commit in the same target window
using their own keys, nonces, and durable journals. Neither coordinator finality nor
another validator's commitment is a prerequisite. Model artifacts are admitted
through on-chain digest commitments and private uploads; the coordinator
publishes every admitted artifact when its cohort's scoring starts.

Every inference attempt on cohort text has a content-free receipt in R2. The
coordinator's signed score package binds the receipt index, object digests,
count, and commitment root. Receipts are coordinator-private audit evidence,
never published or sent to validators; the coordinator installs a new target
only when that cohort's receipt index verifies.
Detailed receipts and the index expire after two subsequent cohorts complete
and the cohort's target is superseded.
Compact score inputs and arithmetic records, certificates, roots/counts, calculated
rows, and chain/accounting recovery state remain. Coordinator silence never
advances this retention window. Validators' commitments need only the signed
row, so deleting receipts never stops them.

The coordinator publishes a signed calculation target and activates its successor
only when the complete signed score package and retained receipts are available,
at the next eligible tempo boundary; validators then re-commit its signed row
every tempo. Publication of newer scores alone does not invalidate the current
target. Its reward-intent authorization never expires: only a newer target or
a signed revocation ends it, and registration checks still apply. The coordinator later
certifies its finalized effective row against the calculation. This provides
consistency evidence without gating another validator's commitment or reveal.
The coordinator's row is published off-chain as soon as it is signed and
follows from the public quantized result, so it is known before any reveal;
enabling native commit/reveal neither prevents copying nor proves
verification.

See the [scoring policy](../whitepaper/sections/07-rewards.tex) and
[cohort lifecycle](../whitepaper/sections/07b-cohorts.tex) for the required
records and transitions. A signature authenticates the coordinator's assertion;
it does not establish honest scoring or Cloudflare execution independently.

## Recovery and accounting

Cohort progress is durable and resumes after coordinator recovery. Scheduling
targets may extend to later tempo boundaries; elapsed time alone cannot expire
or skip a cohort. Coordinator-owned scheduling budgets stop during silence.
Transitions finish without waiting for coordinator certification or validator adoption;
weight delivery is tracked separately. Other validators affect
timely native weight adoption only, with no application-level approval or
recovery gate. Publication and observed payment are separate records.
Subtensor's native emissions continue during coordinator pauses.

Parallel commitments avoid sequential reveal stages between validators. Row
verification, inclusion, native reveal timing, finality, and rate limits still
take time; there is no guarantee of a same-tempo or atomic switch. Timelocked
commitments may reveal automatically before the coordinator's later audit. A missed
window preserves cohort results and records delayed local delivery; after
reconciliation, a node uses the current authorized target in an eligible window.

Journal chain intents before submission and reconcile against finalized state
before retrying. A local timeout is insufficient evidence that a transaction
failed. The [native adapter](../subnet/README.md) documents implemented restart,
weight/reveal, live-metadata resolution, and pause controls together with its
remaining integration gaps. Evaluation semantics never depend on the live
runtime: an upgrade defers only an operation it leaves without a type-correct
encoding and never invalidates a cohort.

The coordinator's primary heartbeat is an authenticated `rtao-rpc` webhook on
the finalized stream, which delivers every finalized block as its own Worker
invocation. The upstream Finney and testnet services are live; the testnet
receiver is integrated in the [coordinator](../subnet/coordinator/README.md),
and its mainnet qualification remains SlopNinja's work. Persist the delivery
before acknowledging it; verify chain identity and finality through RPC rather
than trusting an event to authorize a transition. Maintain a durable finalized
block cursor, deduplicate delivery, and backfill height gaps through bounded
RPC. Due work is claimed through durable leases, so overlapping invocations do
not repeat it. A one-minute cron watchdog is the only fallback: after five
minutes (configurable) without a valid delivery, or when the cursor lags the
finalized head by that threshold, it processes blocks from the public
OpenTensor RPC through the same per-block path until webhooks resume, and
records mode transitions durably. The nominal wait is one finalized block plus
delivery latency, or up to the threshold plus one tick in fallback; RPC/finality
latency and catch-up add to actual observation delay. This is no hard
outage-recovery bound. A late delivery cannot skip a cohort, reset its remaining
active-time budget, or sign a duplicate attempt. The coordinator's Workers
transport, signing, and Durable Object journal backend still require integration
and qualification; the native file journal does not supply them.

Recovery retains each pending commitment's original identity and reveal evidence.
A later target cannot erase that obligation or authorize a stale overwrite of
newer weights. Reconcile submitted and uncertain operations before further
signing, including across runtime changes and ordered migrations. A mismatch in
another validator's row is recorded for correction and never blocks a node with
valid local inputs and authority, or the coordinator's progress.

Customer delivery has its own cutoffs and refund liabilities. An evaluation
pause does not extend customer retention or delivery terms. The
[paid-service guide](paid-inference.md) summarizes that lifecycle and its
pre-launch requirements.

## Privacy and disclosure

Keep customer content and derived artifacts inaccessible to miners. SlopNinja
and Cloudflare remain trusted with content. Public records carry content-free
accounting and salted commitments; private storage holds authorized evidence,
openings, and encrypted results. Other validators receive customer text only under a
separate customer-directed disclosure. The
[privacy contract](../whitepaper/sections/05-confidentiality.tex) defines the
later private-*B* serving conditions and outstanding owner-logging tests.

## Existing code

The [offline subnet prototype](../src/subnet.rs) checks supplied challenge,
submission, detector, and quality-audit records. Its `unslop-v1` reward helper
implements a binary acceptance rule. It authenticates no declarations,
maintains no replay ledger, and is not the current network reward engine.

The [receipt-envelope library](receipt-envelope.md) encrypts and authenticates
opaque payloads under its version-1 schema. Its supplied assignments and key
records require external authentication. It implements neither a network
mailbox nor delivery certification. The
[Pangram report reader](../examples/verify_public_pangram.rs) checks retrieved
provider responses under the limitations in the
[report evidence guide](subnet-receipts-and-audits.md).

These prototypes do not implement the current Cloudflare service, payment
settlement, or coordinator record schema. Their versioned schema identifiers
bind the executable examples and fixtures; production encodings require separate
qualification.

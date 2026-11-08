# Paid inference

Whitepaper draft 0.48 proposes hosted *A* first, with paid access opened at the
coordinator's discretion after quality and service qualification. SlopNinja
serves the public model. Later private *B* serving requires separate activation.
The [whitepaper's paid-service section](../whitepaper/sections/06-paid-inference.tex)
and [Cloudflare design](cloudflare-subnet-draft.md) define the proposal; no
production payment or delivery service is implemented by this guide.

## Pricing and funding

The operator quotes a fixed price for a bounded workload. Its reference model
cost includes every model invocation needed for successful delivery, including
ambiguity resolution and failed attempts. The exploratory multiplier is 25,
subject to measured cost and competitive pricing. It sets the amount destined
for burning, not a cash margin. The paper's dated detection and revision cost
scenarios remain estimates; each actual offer needs measured limits and costs.

Customer inference principal follows these routes:

```text
USD card payment -> native TAO -> subnet alpha -> burn
TAO payment -> subnet alpha -> burn
alpha payment -> burn
```

Coinbase Onramp is the selected candidate for card-to-native-TAO funding.
Supported payment methods, jurisdictions, settlement, and refunds still need
qualification. Solana is preferred for additional crypto entry routes if a
suitable native-TAO route qualifies. Card authorization or an expected swap
cannot fund execution before confirmed arrival.

The operator pays processing, conversion, and chain fees separately so they do
not reduce the inference principal promised for burning. Customers pay their
wallet submission fees unless an offer sponsors them. Miners fund training;
SlopNinja funds public *A* serving and shared evaluation. Later *B* miners
would pay their own Cloudflare bills for customer calls and bounded mandatory
evaluations. Required candidate external-detector checks have a separate prepaid
budget. `rtao-rpc` has no service fee; chain and caller-runtime costs remain.

Burning the full inference principal is a deliberate policy tying paid demand
to an alpha supply sink. Its economic optimality is not established. The
principal funds no direct miner or operator payout. Operating bills and refunds
require spendable capital or realized proceeds from a separately allocated source. A future paid-use emission pool is a
separate policy choice; burned tokens cannot guarantee its value or solvency.

## Offer and job lifecycle

1. Quote the model and pipeline version, workload and attempt limits, fixed
   price, currency, cutoffs, retention, and refund terms.
2. Reserve confirmed funds, capacity, and refund liquidity in durable state.
   Bind a customer idempotency key before dispatch.
3. Validate the input and customer output key, run the admitted pipeline, and
   retain every bounded attempt. A provider timeout leaves execution uncertain
   until reconciled; retries may incur another provider charge.
4. Store the validated result encrypted to the customer's key and verify
   retrieval under the quoted delivery terms. Issue an operator-signed receipt.
5. After valid delivery and the pre-burn refund window, allocate the principal
   to a finalized burn once. Return excess funds and enforce retention limits.

The delivery predicate, refund window, canonical receipt encoding, and native
alpha burn permissions must be qualified before paid launch. A result hash
alone cannot authorize a burn. Refundable custody remains fully funded even
if conversion precedes delivery. After burning, any refund is a new payment
from operator reserves.

Durable Object ledgers preserve reservations, attempts, and settlement. Queues
carry opaque job IDs. Restarts and evaluation-cohort pauses do not extend
accepted customer deadlines or suspend delivery and refund obligations.
Replacement models require prior authorization under the accepted offer or a
new quote. The original attempt stays in the record.

## Customer confidentiality

The [current privacy contract](../whitepaper/sections/05-confidentiality.tex)
trusts Cloudflare and SlopNinja with content while excluding miners from
customer prompts, responses, and derived representations. Usage metadata may
be visible. Later private *B* weights must remain unavailable to the public;
SlopNinja may technically access them. The proposal makes no TEE claim.

Customer ciphertext, billing identities, raw document hashes, and provider
report URLs stay off-chain. Public receipts contain content-free accounting
and salted commitments. The
[receipt-envelope prototype](receipt-envelope.md) documents a useful encryption
primitive with a version-1 schema; it does not implement this service lifecycle.

## Optional customer-directed inspection

A dispute gives validators no automatic access to customer text. A separate
authorization names the recipient, purpose, evidence, and disclosure window.
External detectors and training reuse each require their own consent.
Partial disclosure supports only a limited finding. Refund remedies follow
the accepted service terms.

## Paid-use credit

Any future inference allocation requires an admitted model version, qualifying
delivery, and finalized burn allocation. Pending burns retain pending credit.
Failed attempts, free traffic, mandatory evaluations, duplicate receipts, and
extra tokens earn none. Work units and the epoch assignment rule are frozen
before admission. Public *A* additionally needs an explicit model-author versus
serving-cost credit decision. See the
[reward policy](../whitepaper/sections/07-rewards.tex) and
[economic analysis](../whitepaper/sections/06a-tokenomics.tex).

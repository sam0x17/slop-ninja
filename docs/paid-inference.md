# Paid inference

Whitepaper draft 0.66 proposes hosted *A* first, with paid access opened at the
coordinator's discretion after quality and service qualification. SlopNinja
serves the public model. Later private *B* serving requires separate activation.
Before mainnet, a free detection page, paid checks of longer documents and
later a paid detection API earn revenue for a public pot ([below](#revenue-before-mainnet)) that pays the
mainnet registration, then buys alpha to burn and to distribute to testnet
miners by the detection requests their models served after beating the
reference; paid jobs, the
quoted and reserved workloads this guide describes, keep the per-job burn
route below once the subnet's alpha exists.
The [whitepaper's paid-service section](../whitepaper/sections/06-paid-inference.tex)
and [Cloudflare design](cloudflare-subnet-draft.md) define the proposal; no
production payment or delivery service is implemented by this guide.

## Revenue before mainnet

Testnet runs for several months before the mainnet subnet is registered. The
first product is a free detection page at `/detect/`: a visitor pastes text,
or the address of a public web page or post, without an account, and
SlopNinja's span package d24 scores it in SlopNinja's own runtime, so the text
reaches no miner. Every text of 24 words or more gets one of four evidence
bands with its confidence, whose measured rates the
[detect evidence](detect-evidence.md) publishes: from 150 words under the
package's own map, and from 24 to 149 words under a confidence map and points
fitted for each length range on Calibration-split excerpts
([detect evidence](detect-evidence.md#short-texts)), without the share. Spans
of at least 24 words are highlighted only in the two strongest bands; a longer
text is scored on its first 1,000 words, cut at a sentence end
([detect evidence](detect-evidence.md#long-texts)); the page scores no text
under 24 words. Every check gets a page whose link never expires: a checked
public web page or post gets an indexable public result page unless an opt-out,
a removal or legal state, or a family filter requires exclusion. Search engines
control actual indexing. A pasted text gets an unlisted page, excluded from
listings and sitemaps and carrying instructions not to index it, encrypted under that page's
key, at a short link derived from the text, so the same text always reaches
the same page; a post on X keeps only its id and the result. Accounts are
optional, and every result page links to a removal form. No advertising or
analytics script runs on the check page or on any page that holds pasted text;
ads run only on approved content pages. SlopNinja LLC, a Maryland limited
liability company, operates the site. Paid checks score longer documents whole
and bill only the tokens past the free cut; at launch only operator credit
grants back them. A paid detection API follows once the
package is published. These products are priced on their own (pay-as-you-go
credit in fixed reload sizes that never expires, or a monthly plan whose
allowance resets each month), paid through a merchant-of-record card processor
or directly in TAO, and settle through the pot rather than one job at a time.

Later, the detection products are to serve the reigning reference model
whenever it passes serving checks published before the pot's first deposit,
so that a miner's model that has beaten the reference serves the products'
requests and is credited with them.

Everything the detection products' advertising networks, sponsors and payment
processors pay out goes into the pot, net of the fees they deduct at source;
SlopNinja pays its own costs separately and never from the pot, except the
mainnet registration below. The pot is native TAO in a published two-of-three
multisignature account with an independent third signer, with a public page
listing every payout, conversion, balance, registration, purchase, burn and
claim, the detection requests counted for each model, and a signed monthly
statement. Its money is used in three steps:

1. **Registration.** Once the pot holds about 1.5 times the cost of a mainnet
   subnet registration, SlopNinja registers the mainnet subnet with that
   money. Until then the pot's TAO is neither staked nor spent. Any part of
   the registration cost later returned to the subnet's owner account, for
   example when the subnet is dissolved or deregistered, goes back to the pot
   and is listed on the pot's public page.
2. **Purchases for the testnet era.** Once the subnet launches (its emissions
   have started), the TAO left from revenue earned before the launch, net of
   the registration, buys the subnet's alpha in daily tranches bound to the
   registration. Half is burned. The other half, the whole amount distributed
   and so already reduced by the registration's cost, goes to testnet miners
   on mainnet through a claim contract, by inference requests: each
   miner receives the fraction of that half that the requests served by its
   winning models (models that beat the reference and took its place) form of
   all requests the detection products served before the launch. Requests not
   credited to a miner's winning model, such as those served while one of
   SlopNinja's own models is the reference, count in that total but credit no
   miner, and their part of the half is burned. SlopNinja's own test requests
   are not counted. A counting rule, published before the pot's first deposit,
   says what counts as one request and which model it is credited to; it, the
   serving checks and the claim contract's rules are published before they
   are used.
3. **After the launch.** All revenue from *A* and later *B* products buys and
   burns alpha, with nothing distributed.

If mainnet is abandoned, the pot's TAO, including any registration cost
returned to it, is burned and nothing is distributed.
The distribution is separate from subnet emissions and changes no score, row
or emission. As of November 15, 2026 the pot has received no money. The
[design notes](cloudflare-subnet-draft.md#revenue-before-mainnet-the-detection-page-and-the-public-pot)
give the full rules, and the [testnet terms](testnet/terms.md#the-pots-planned-distribution)
say what the distribution is and is not. The page's service, the
[detect Worker](../subnet/detect-worker/README.md), is built, with its paid
checks on operator grants, but not deployed to production. The initial release
is paste-only; public URL and X checks remain disabled pending activation,
and the X fetching routes remain unfinished. The payment
processor, API keys and the pot are not yet implemented.

## Pricing and funding for paid jobs

The operator quotes a fixed price for a bounded workload. Its reference model
cost includes every model invocation needed for successful delivery, including
ambiguity resolution and failed attempts. The exploratory multiplier is 25,
subject to measured cost and competitive pricing. It sets the amount destined
for burning, not a cash margin. The paper's dated detection and revision cost
scenarios remain estimates; each actual offer needs measured limits and costs.

The principal of a paid job follows these routes:

```text
USD card payment -> native TAO -> subnet alpha -> burn
TAO payment -> subnet alpha -> burn
alpha payment -> burn
```

For paid jobs, Coinbase Onramp is the selected candidate for card-to-native-TAO
funding; the detection products use the processor or direct TAO payments above.
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

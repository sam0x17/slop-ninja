# Pangram comparisons and API funding

Whitepaper draft 0.64 and the [Cloudflare design](cloudflare-subnet-draft.md)
define the current proposal. The coordinator runs the hidden *A* comparison and
signs the weight row; other validators verify that signed row and commit it
through native commit/reveal without held-out data or exact scores. The coordinator's finalized
row is checked afterward. *B* is deferred. This guide describes the comparison
policy and supporting provider evidence.

## What we measure

Compare admitted *A* models, the incumbent quality anchor, and Pangram on the
same independently labelled hidden population. Freeze versions, population
weights, operating points, repetition policy, uncertainty analysis, and budget
before opening the set. Report human false positives, sensitivity, coverage,
failures, and source-family uncertainty. Agreement with Pangram supplies no
ground-truth label or separate reward credit. Author attribution needs its own
reference/query works and documented identities.

Pangram's current fractions for AI-generated, AI-assisted, and human text are
distinct from *A*'s probability of any AI involvement. The public dashboard
explanation uses character-based shares; our share target uses final words in
recorded model-produced spans. Quantitative share comparisons need common span
labels and counting units. The provider's fractions cannot substitute directly
for the calibrated probabilities used in *A*'s Brier reward.

Pangram begins as the external quality anchor: the reference comparator for
measured quality claims and the coordinator's paid-service activation decision.
Anchor status supplies neither a reward weight nor an automatic activation
gate. A contender must exceed the incumbent by a predeclared practical margin
at declared false-positive operating
points, satisfy slice limits, and confirm the result on fresh material after
selection. Ties, missing observations, and inconclusive comparisons retain the
incumbent. The coordinator publishes a signed, version-bound decision for a
future cohort. A new anchor cannot change a frozen reward baseline or issued
comparison. Continue measuring Pangram after a subnet model becomes the anchor.
See the [external-comparison policy](../whitepaper/sections/08-pangram-audits.tex).

Later *B* research jointly measures author fit, preservation, readability, tone,
and detector changes. Its proposed strict Pangram target is
`fraction_ai + fraction_ai_assisted < 0.10`; incremental improvement does not
establish that target or a successful revision. Report source and candidate
observations under the same frozen policy. Panel construction, report selection,
authority, and service duties must qualify before *B* activates. The
[later B proposal](../whitepaper/sections/07a-adjudication.tex) owns those rules;
there is no joint A/B launch schedule in the current design.

## Exact observations and provider trust

Use an explicit supported model selector and bind exact request bytes, returned
version, task/item IDs, timestamps, settings, and full responses. The historical
probe used `pangram-4`; qualify the intended selector before new measurements.
The [Bulk API](https://docs.pangram.com/api-reference/bulk-api) exposes item/result
bindings. Provider selectors and returned versions are exposed identities, not
cryptographic proof of immutable weights.

The [public-result probe](pangram-public-results.md) retrieved one synthetic
report, including its full analyzed text, fractions, version, and timestamp,
without credentials. The API and report removed a trailing LF. That observation
requires exact-byte controls; it does not authorize silent normalization after
commitment. The public web endpoint remains an undocumented implementation
detail requiring supported access and retention terms before deployment.

The probe established no provider signature or immutability guarantee. Direct
retrieval observes Pangram's stored response; copied JSON or a screenshot alone
does not establish provenance to another party. Pangram remains trusted for its
computation, availability, and billing. Provider agreement cannot prove human
authorship or editorial quality.

Customer text reaches Pangram only through a separately authorized and funded
external check. Report URLs, provider text, and exact request commitments stay
in authorized private evidence storage. Anyone with a public report URL may
read its text. Encrypting that locator during delivery does not add access
controls at Pangram. The [receipt-envelope prototype](receipt-envelope.md)
implements its existing schema, not a deployed audit or customer-service system.

The [TLS fallback probe](pangram-public-results.md#tls-fallback-probe) produced a
5,513-byte TLSNotary presentation for a GET of Pangram's models list using a
local notary. It did not prove a detection submission and result or establish
independent-notary costs. A separate September 9, 2026 connection check negotiated
TLS 1.2 at `text.external-api.pangram.com` with a verified certificate and received
401 without credentials. These are bounded transport observations, not qualified
response evidence for the subnet. No transcript-proof mechanism is required by
the current coordinator-led pilot.

## Reproducibility spot checks

The [repeatability probe](pangram-repeatability.md) covered three texts with
three fresh requests per text through one account. Document class fractions
matched within each text while auxiliary scores varied. Two selected
intermediate-fraction texts also matched older document fractions; same-task
GETs on the first text matched its original result. The nearest observation
was 14.80%, with none within two percentage points of 10%. This does not
establish determinism near the threshold or cross-account reproducibility.

A GET of the original task retrieves that observation. A fresh POST measures
the text again and may incur another charge; it neither authenticates the
original response nor proves the original evaluator incurred a charge. Keep
these operations distinct. Compare a frozen projection of completion, returned
version, and document fractions; retain task IDs and timestamps for provenance
without requiring them to match across tasks.

An absolute agreement tolerance of `0.02` remains an unvalidated research
candidate. A 9.5% result and an 11% result differ by less than two percentage
points, but 11% still fails the proposed strict 10% target. An agreement
tolerance does not change reward or quality thresholds. A different fresh
result does not by itself invalidate a stored observation or establish fraud.

For an optional study sampling `N` observations at fraction `q`, with `r` fresh
requests each, budget `q * N * r` additional evaluations at the applicable
length-based tariff. Freeze the sample, attempts, analysis, and budget first.
Keep failed and unresolved requests visible; do not retain only favorable
repeated results.

## Who pays

SlopNinja funds the shared *A* provider observations once per comparison set;
adding an admitted model need not multiply that bill. It also funds central
*A* execution and the declared shared review workload. Miners fund private
experimentation. Later *B* miners would fund their bounded evaluation calls and
required candidate reports through a prepaid budget; the operator would fund
shared source baselines. Report counts and selection policy remain activation
choices. There is no selected mandatory three-report or 24-task launch schedule.

Customer-authorized external checks use a separate disclosed budget and do not
reduce the inference principal promised for burning. Optional repeatability or
proof research needs separate funding. Operating capital and explicitly
allocated realized proceeds fund operator work; burned principal cannot pay
provider invoices. Actual billing and any reimbursement require retained task
and charge records rather than miner-supplied query counts.

The dated planning rates are $0.05 per started 100-word realtime unit and a
20% bulk discount, with at least one unit per item. The bulk documentation
reviewed for that estimate allowed 1,000 billable units per job. Recheck current
prices, limits, and negotiated terms before allocating a live budget:
[API pricing](https://www.pangram.com/solutions/api),
[bulk billing](https://docs.pangram.com/api-reference/bulk-api).

For an illustrative workload of 24 tasks, one source and one revision per task,
exactly 500 billable words per text, and three observations per text:

```text
24 * 2 * 3 * ceil(500 / 100) * $0.05 = $36 realtime
$36 * 0.8 = $28.80 with the listed bulk discount
```

This is a cost example, not a launch schedule. It excludes human review, proof
computation, optional studies, and any extra billable attempts. Reserve budget
before dispatch and reconcile task IDs, failures, and charges without duplicate
credit. Provider outages retain unresolved comparison work under the cohort
recovery policy; they do not justify favorable substitutions or a smaller
comparison population. Supported retrieval, retention, version binding, and
the complete funded workload still require qualification.

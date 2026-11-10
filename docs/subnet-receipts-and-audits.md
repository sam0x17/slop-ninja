# Receipts and external-detector evidence

Whitepaper draft 0.63 uses coordinator-led *A* evaluation and operator-signed
service receipts. The [Cloudflare design](cloudflare-subnet-draft.md) and
[service-evidence guide](service-evidence.md) describe their authority and
recovery boundaries. This page records the existing receipt/report utilities
and the limits that still matter when using them.

The coordinator is the operator's lead validator and the serverless service.
Its signed score package and receipts are coordinator-private audit evidence,
never published or sent to validators. Other validators verify and commit the
coordinator's signed row; anyone may recompute that row from the public
quantized result as an advisory check, and no validator receives the
coordinator's private evaluation data.

## Current receipt proposal

Evaluation records bind the cohort, model and pipeline versions, input
commitments, outputs, and scoring calculation. Customer receipts additionally
bind the accepted job, output key, encrypted result digest and length, attempt
record, and delivery terms. Payment and burn records have their own linked
state. The exact production encodings and delivery predicates need
qualification before receipts authorize money or paid-use credit.

Public receipts exclude customer text, raw document hashes, billing identities,
and provider report URLs. Content commitments use fresh secret salts. Keep
openings and private evidence with authorized recipients. A commitment proves
consistency when opened; a signed receipt records its signer's assertion.
Neither proves honest execution, correct semantics, or result availability.

The [receipt-envelope prototype](receipt-envelope.md) implements HPKE encryption,
signed key records and envelopes, and commitment-opening checks using a
version-1 schema. Its exact encoding is documented because live examples and
tests use it. It has no chain registration, persistent replay ledger, mailbox, or
current service-delivery state machine.

## Pangram report reader

The Rust [report reader](../examples/verify_public_pangram.rs) retrieves a
provider response and checks the supplied text binding and expected report
fields. Preserve the raw response and apply a versioned score projection.
The [public-result probe](pangram-public-results.md) documents the observed
endpoint and its limitations; [repeatability results](pangram-repeatability.md)
cover only the measured texts and account.

The observed `web.pangram.com/api/history/<id>/` endpoint returned text and
results without authentication. A report URL therefore exposes its analyzed
text to anyone who obtains it. Encrypting the URL in SlopNinja's transport
cannot make the provider endpoint private. Customer use requires separate
consent; hidden benchmark material needs an authorized provider path and
retention policy.

The probe observed a null `model_id` and removal of a terminal newline. Exact
accepted-byte comparison is necessary when binding a report to a candidate.
The later *B* comparison retains a canonical text profile in its
[evaluation proposal](../whitepaper/sections/07a-adjudication.tex). Provider
changes, output normalization, and retrieval failures require explicit handling;
no immutable or cryptographically signed provider receipt was established.

Repeated reads of one report remain one observation. Repeated queries can
produce different observations, and selectively choosing favorable reports
can bias a result. Any rewarded policy must freeze allowed calls and selection
before candidate generation. A low Pangram score establishes neither human
origin nor preservation, readability, or author fit.

## Use in current research

The [A anchor policy](../whitepaper/sections/08-pangram-audits.tex) compares
external and subnet detectors against independent labels with declared metrics.
Pangram's fraction and *A*'s involvement confidence are different measurements;
any comparison or transformation must state its interpretation.

Later *B* research can combine external observations with source/candidate
comparisons and complete semantic review under the
[style utility contract](reward-adjudication.md). Its scoring authority and
activation remain open. These local utilities support report retrieval and
payload encryption; the comparison and service protocols require their own
implementation and qualification.

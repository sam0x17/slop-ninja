# Public Pangram results as validator evidence

On 2026-09-09, one `pangram-4` submission with
`public_dashboard_link: true` returned a public result that another caller
could retrieve without an API key or cookies. Both the page and its JSON
endpoint returned HTTP 200. The JSON included the full analyzed text, document
fractions, version `4.0` and a timestamp. This gives validators a way to check
the provider's stored observation without submitting another paid inference.

The [documented option](https://docs.pangram.com/api-reference/ai-detection)
produced a synthetic demonstration report at a public history URL
(`https://www.pangram.com/history/<history id>`; its history id is recorded
privately as `redacted-c8465873f9c7defd`).
It scored 100% AI. This experiment tested public retrieval, not successful
evasion. The additional 126-word submission is two billable units at the list
price, an estimate.

The public page's own JavaScript retrieves:

```text
https://web.pangram.com/api/history/<history id>/
```

This web endpoint is an observed implementation detail. Its schema, retention,
rate limits and availability need a supported agreement before a live subnet
depends on it. A public report exposes the text to anyone who has its URL;
this probe used nonprivate synthetic material.

## Exact binding and limitations

The complete text was present in `prompt` and `response_payload.text`.
`response.overall.text` contained only the first 300 characters; validators
must not use that preview for a full-text comparison. The API and public report
removed the submitted trailing LF. Our reader requires exact equality with
the committed candidate and rejects a mismatch. It never silently normalizes
a candidate after commitment. Longer, paginated reports remain untested.

The public record contains reported version `4.0`, but `model_id` was null.
It therefore cannot prove the original requested selector or immutable model
weights. The record identifies one stored run. No text-hash lookup, unique
canonical result per text, cryptographic provider signature, or immutability
guarantee was established. Validators trust Pangram when they fetch its record
directly. A saved JSON file or screenshot alone does not establish provenance
for an offline third party.

A miner can demonstrate that Pangram reported zero AI and zero AI-assisted
fractions for a particular candidate. Meaning preservation, factual fidelity,
authorship and writing quality require separate evidence. Randomness and
commitments can organize those checks but cannot make their judgments true.

## Private evidence handling

The current [external-comparison policy](../whitepaper/sections/08-pangram-audits.tex)
keeps report URLs and analyzed text in authorized private evidence storage.
Keep raw report IDs, sensitive scores, and evidence spans out of public
submissions and logs. Deterministic unsalted text hashes can disclose guessed
inputs, so they also require private handling. Customer text reaches Pangram
only through a separately authorized external check.

Pangram still receives and hosts the text, and an authorized reader can copy
the URL or plaintext. No
confidential-hosting, search-exclusion or non-disclosure guarantee was
established. Stronger privacy would require provider access controls or a
different verification integration. The
[receipt-envelope prototype](receipt-envelope.md) implements encryption under
its version-1 schema. It supplies no access-control guarantee for Pangram's
public report endpoint and does not implement the production service.

## Rust reader

[The public-result reader](../src/public_result.rs) takes a report UUID,
constructs a fixed provider URL, disables redirects and retries, and bounds the
response size and timeout. It requires the expected text, reported version,
public access and evaluation time window. It checks agreement between the two
public score views and retains the strict AI-plus-assisted threshold.

The example only retrieves an existing result. Set the expected candidate file
from your own committed bytes, not from an untrusted miner-supplied preview.

```sh
cargo run --release --example verify_public_pangram -- \
  --report-id HISTORY_ID \
  --text data/pangram-proof-v1/public-candidate.txt \
  --version 4.0 \
  --not-before 2026-09-09T00:00:00Z \
  --before 2026-09-10T00:00:00Z \
  --out data/pangram-proof-v1/public-reader-run
```

Use a new output directory. The example saves the provider response before
validation, including on a binding failure. This is an experimental reader;
assignment authorization, replay accounting, validator consensus and editorial
review are outside its scope. Its local output contains the report locator and
provider text and must remain in the authorized validator's private storage.
The example does not encrypt transport payloads or local output files.

## TLS fallback probe

A separate ignored Rust prototype using TLSNotary `0.1.0-alpha.15` successfully
proved an authenticated GET of Pangram's `/models` endpoint with the API key
hidden. The 5,513-byte presentation verified offline under a pinned local
notary key. One local run took about 0.35 seconds, including both parties;
this does not estimate independent, remote-notary costs.

The larger initial transcript limits exceeded the upstream session's stream
capacity. Using the upstream example's 4,096 sent / 16,384 received limits
resolved setup. Three focused offline tests, formatting and Clippy passed.
No submission/result proof was attempted. The local-notary experiment does not
establish notary independence, hidden-header syntax, freshness or replay
resistance against a malicious prover. Public report retrieval is the simpler
initial mechanism to investigate.

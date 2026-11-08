# SlopNinja subnet

The [whitepaper](../whitepaper/slop_ninja.pdf) is the canonical proposal for the SlopNinja subnet. This page names the repository components that support that proposal and marks the implementation boundary.

The [Cloudflare design notes](cloudflare-subnet-draft.md) contain implementation
detail for the whitepaper, including serving, billing, recovery, and
operator-signed receipts. Testnet participation is described in the
[testnet guides](testnet/README.md).

## Scope

Launch includes A alone: one text model uses origin detection and attribution
to assess AI involvement in writing, returning estimated AI share and AI
confidence as separate percentages, AI spans with per-sentence share and
confidence, and conditional attribution. Word-level explanations are a later
A extension; grammar-level explanations belong to later B revision
guidance.
The operator's lead validator is the coordinator, implemented entirely as
the serverless service. It executes hidden evaluations, publishes signed per-miner
scores, and performs its own chain submissions and recovery. Other validators
independently calculate canonical weight rows and use native commit/reveal
without receiving held-out data or waiting for the coordinator's row.
Finalized rows are checked afterward. Rewards follow `reward-v1`: daily cohorts
scored against a public reference model that changes only on improvement, with
SN78-style geometric bands. A reads raw text and uses no CFG inputs.
B would later revise text toward an authorized voice with preservation and
editorial evaluation, using the native grammar for edit units and revision
guidance; C would credit accepted improvements to that grammar once.

A artifacts stay private while their cohort runs, then are published and can be
self-hosted. The proposed later B path uses
private adapters in miner-owned Cloudflare accounts, with SlopNinja controlling
direct requests. Content must remain inaccessible to miners and private weights
to the public; Cloudflare and SlopNinja are trusted operators. Supporting
guides follow this proposal and identify the retained prototypes' separate
implementation limits.

## What the repository provides

- The [V6 detector baseline](../baseline/detector/README.md) is a public research artifact. Pangram remains the outside quality anchor.
- The [B baseline](../baseline/transformer/README.md) studies source-bound edit menus, grammar and lexical measurements, and semantic review.
- The [grammar workspace](../grammar/README.md) holds the native grammar and parsed representations for later B work, plus the parser-free A crates.
- [Receipt-envelope code](receipt-envelope.md) demonstrates encrypted, commitment-bound delivery. It is not a live chain integration or a qualified confidential-serving system.

## Implementation boundary

The [native chain integration](../subnet/README.md) pins `rtao` and supplies a
durable weight adapter with journal controls and restart recovery. It is an
embedding library, not a deployed validator or coordinator. Its integration
gaps include application authority, by-name composition of weight calls in
upstream rtao, and journaled MEV-shield dispatch. The coordinator also needs a qualified Rust/Wasm chain
signing path and Durable Object journal backend. Live rtao-rpc finalized-block
webhooks are its per-block heartbeat, with RPC gap backfill and a one-minute
cron watchdog that falls back to public OpenTensor RPC when deliveries stall;
the [testnet coordinator](../subnet/coordinator/README.md) implements that
receiver on subnet 601, and mainnet qualification remains.

Apart from the testnet subnet 601 pilot, the repository does not yet run a subnet, submit validator weights, operate a customer service, or qualify a confidential B runtime. The local Rust contracts and evidence tools support research and implementation work. They do not establish production security, semantic preservation, detector parity with Pangram, or a right to use a source corpus.

## Reading order

Start with the [whitepaper](../whitepaper/slop_ninja.pdf), then the
[design notes](cloudflare-subnet-draft.md) and [native adapter](../subnet/README.md).
The [documentation index](README.md) links current model, service, and evidence
guides alongside the retained research contracts and executable prototypes.

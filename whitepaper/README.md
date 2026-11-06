# Whitepaper

[Draft 0.61](slop_ninja.pdf), November 6, 2026, describes a subnet whose main
objective is B: transforming authorized writing toward the author's intended
voice while preserving meaning, readability, and tone. A supplies supporting
detection and attribution signals for that editing process and launches first.
The intended training is adversarial: B learns against public A detectors,
while authorized B revisions supply challenging examples for future A training
after evaluation retirement. Improvement must be measured on fresh held-out data.

The A-only launch uses one text model that reports document-level AI share,
AI-involvement confidence, AI spans with per-sentence share and confidence,
and human/model attribution. Only involvement Brier is rewarded; the other
outputs are qualification gates. Under the protocol, miners commit artifact
digests on chain and upload model files straight to the coordinator's private R2
storage by presigned URL, within a cap of one allow-listed 160M-parameter graph
of at most 700 MB; R2 checks each file's declared SHA-256 on arrival, and no
Slop Ninja code hashes model bytes after the miner packs them. Each day the coordinator publishes the entrants' models, licensed
CC BY-SA 4.0, when scoring starts, scores every entrant and the current and previous
reference models (when a previous one exists) on a fresh hidden batch of paired cases, 20% human-only, 55% mixed and 25%
model-only, with half of the generated text planned to come from frontier
models, in parallel
fan-out lanes, and then publishes quantized scores; exact scores stay private. Mainnet human text
comes from pre-cutoff Common Crawl, a historical-human proxy drawn with secret
keys, capped domain shares, a cohort generation profile that matches generated
text to its human parent's form, and era gates; testnet uses a smaller public
stock and, for cohorts without a working outside entrant, reuses only documents
already exposed, a rule the coordinator enforces. Slop Ninja publishes its own
reference weights under CC BY-SA 4.0 too, with synthetic replay cases and no
evaluation text. Testnet starts from the baseline release candidate and may
replace it once, announced in advance, with the next qualified reference;
otherwise the reference changes only when an entrant beats it. Mainnet opens a
fresh record with the reference testnet holds at that time. Rewards port subnet 78's
geometric band mechanism, computed from quantized scores released with seeded
noise under a per-cohort privacy budget, with no credit for detected copies of
earlier published artifacts (a copy perturbed with deliberate noise can still
escape, at a measured cost of about a tenth of an unrounded band at 1,000 cases), equal splits inside near-duplicate clusters, equal credit among equal quantized
scores, a fallback cutoff against the previous reference, and burns for
unallocated credit. Private writer profiles
for later attribution and B author-fit feedback use A's encoder plus
stylometric features and await a learned matcher. A uses no CFG features; the
shared English grammar is kept as a later B input for edit units and revision
guidance. B transformation and the C grammar contributions that support it
activate separately, and the mainnet emission split is left open.

The current evidence is dated. On an October 30 sealed, pre-registered Test, the
A baseline release candidate was noninferior to Pangram 4.1: balanced Brier 0.0518 against
0.0551 after the same Calibration-fitted map, a paired difference of −0.0049 [−0.0103, +0.0005], and 1.31% against
2.35% human false positives at each detector's own 1% Calibration point. The
pre-registered verdict failed because the three-seed recipe missed the
legal-opinions slice limit; the artifact itself passed every condition on its
own. f32 serving on Cloudflare Containers matches the reference replay. The
release adds a span head that leaves its document outputs unchanged and finds
about 40% of AI-written characters in held-out spliced rewrites, mostly missing
clause and single-sentence edits. A head with an added four-layer encoder
tower, adopted for the first package that serves spans, finds about 53%, at
18–20% more encoder time, but puts spans on more consumed-Test humans. CFG-only inputs failed their first real-data
test, and CFG writer vectors scored below the encoder on writer verification and
added little to encoder plus stylometry, so A uses no CFG features and the
grammar continues as later B work. A Common Crawl pilot found a 3.1%
detector flag rate with no trend across crawl years; its follow-ups reached 72%
usable domain precision and passed both era gates on their point estimates.
A native validator committed, re-committed and recovered weight commitments on
testnet through fault drills without a duplicate, and rehearsal cohorts then ran
`reward-v1` end to end there: signed packages that an offline verifier, the
coordinator and the native validator checked, with rows committed and revealed
as derived. No live row has yet paid more than one UID, validators still trust
the coordinator's published-artifact index, and receipt deletion is not yet
implemented. Dated research results remain
distinct from a qualified model or live service.

The operator's lead validator is the serverless coordinator. It runs hidden
evaluations, signs its own chain submissions, and recovers unfinished obligations.
It keeps held-out data and exact scores private, publishes quantized per-miner
scores, derives the canonical row from them, and publishes that row signed.
Every other validator verifies the signed row and commits it every tempo through
journaled native commit-reveal, and keeps committing the last signed row through
coordinator silence, since no reward-intent authorization expires; its own
checks of models
or the row are advisory. Cohort N is paid during cohort N+1. No validator's delivery
gates cohort progress, and no record or gate depends on a chain runtime version.
Live rtao-rpc finalized-block webhooks are the coordinator's per-block heartbeat,
with a durable finalized cursor, RPC gap backfill, and a one-minute cron watchdog
that falls back to public OpenTensor RPC when deliveries stall, including after
the provider pauses a subscription, which an operator must resume. The webhook
receiver and Workers chain signing remain integration work to qualify.

The tokenomics analysis covers burn demand, cost recovery, emission allocation,
operating liquidity, and self-purchase incentives. Its graphs use the dated
cost scenarios and explicit sensitivity assumptions; they forecast no token
price or emission budget.

The authoritative source is [main.tex](main.tex), with editable LaTeX files in
[sections/](sections/), a bibliography in [references.bib](references.bib), and
an editable SVG diagram and PGFPlots graph sources in [figures/](figures/). The
source and rendered paper are design proposals. [The design notes](../docs/cloudflare-subnet-draft.md)
provide more implementation detail for the coordinator-scored A-only pilot
and the separately activated B and C mechanisms.

Run on the authorized compute host:

```sh
bash whitepaper/build.sh
```

The standard build uses `latexmk`, XeLaTeX, BibTeX, and `rsvg-convert`, with
TeX Gyre Pagella/Heros and Latin Modern Mono fonts. It renders the SVG diagram
and compiles the native LaTeX source, including the PGFPlots graphs. Build output
stays in ignored `build/`. The PDF and PDF diagram are checked in.

Draft 0.61 was built on the authorized compute host with `build.sh` inside a local Docker image
derived from `texlive/texlive:TL2025-historic`, pinned by digest
`sha256:7056390eac37715fd3e01875037ceb4016cb17d44c611f7bbbfa7a012007979f`,
with Debian's `librsvg2-bin` 2.63.3 added and the XeLaTeX format prebuilt. The
worktree was mounted, the container ran without network access as the invoking
user, and no other host or GitHub Actions compute was used:

```sh
cat > Dockerfile <<'EOF'
FROM texlive/texlive:TL2025-historic@sha256:7056390eac37715fd3e01875037ceb4016cb17d44c611f7bbbfa7a012007979f
RUN apt-get update && apt-get install -y --no-install-recommends librsvg2-bin && rm -rf /var/lib/apt/lists/*
RUN fmtutil-sys --byfmt xelatex
EOF
docker build -t slop-ninja-whitepaper-tex:TL2025-historic-rsvg .
docker run --rm --network none -u "$(id -u):$(id -g)" -e HOME=/tmp \
  -v "$PWD/whitepaper:/w" -w /w slop-ninja-whitepaper-tex:TL2025-historic-rsvg bash build.sh
```

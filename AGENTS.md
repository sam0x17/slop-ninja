# Project conventions

The project is named **Slop Ninja**, with **slop.ninja** as the planned domain.
Use **Slop Ninja** in project-facing prose and `slop_ninja` in filenames and
command names. The local repository directory is `slop_ninja/`; the GitHub
repository slug is `slop-ninja`. Existing versioned `slopninja` schemas and
cryptographic domain identifiers retain their spelling for compatibility.
Existing `unslop` command names, package names and versioned
artifact schemas remain compatibility identifiers. Describe the whitepaper and
design notes as one self-contained proposal, without references to previous or
superseded designs. Preserve dated empirical evidence and implementation limits.
Keep documentation focused on current behavior. Delete superseded plans and
experiment diaries; Git retains history. Preserve data and fixtures still used
by executable checks or current evidence.

The assets in `assets/brand/` are the approved Slop Ninja logo, not a proposal.
Project code and docs are MIT OR Apache-2.0, except files and datasets with
their own licence or notice; miner submissions and reference weights are
CC BY-SA 4.0, and the `assets/brand/` logo is not covered by the code licence.
Slop Ninja publishes its own references (R0 and later) like any R_k; a public
reference package carries no evaluation text (synthetic replay cases, never
Dev-iter ones) and passes a package-corpus scan. Paid A serves a published,
qualified artifact (R0 once its public package is published).
Update the whitepaper's baseline evidence at meaningful evaluated milestones,
not after every experiment. Keep both A and B's measured limits explicit.
Whitepaper draft 0.60 and `docs/cloudflare-subnet-draft.md` describe the current plan: launch A
alone; B transformation and C (shared contributions to the grammar B uses)
activate separately, each by its own decision, and C may activate before B so
the grammar can grow first. A reads raw text; the native CFG is a later B input
for edit units and revision guidance, with grammar-level writer-style features
a candidate B input qualified on B's own evidence. Present CFG in public pages
and docs as later B work, not as part of A.
B is the main product objective: authorized writing transformation toward the
author's intended voice with meaning, readability, and tone preserved. A is a
supporting detection and attribution mechanism and the initial standalone
service. The intended A/B training loop is adversarial: B trains against public
A detectors, and authorized B revisions challenge future A training after
evaluation retirement. Measure gains on fresh held-out data; customer material
is not reused by default. Keep the product objective distinct from the A-first
launch sequence.
A is one model with a shared learned encoder for origin, AI share, and
human/model attribution outputs. Report two document-level percentages:
estimated AI-written/rewritten share and confidence in any AI involvement.
Confidence is the calibrated probability of involvement, trained from binary
labels; share estimates the expected final-word fraction in model-produced
spans and needs measured fractional supervision. Keep expected share between
model-only probability and involvement probability. There is no required boolean output.
A also localizes AI text: a token-level head gives per-token AI probabilities,
decoded into AI spans with exact character offsets and per-span confidence, and
per-sentence AI share and confidence derive from the same probabilities,
consistent with the document outputs. Document confidence Brier is the only
reward; share, attribution and spans are qualification gates only, with span
thresholds fixed after measurement. The release is R0 (seed 17) plus the S0+
token head, with document outputs bit-identical to R0; S2c is the adopted span
head for the first package that serves spans (`docs/a-spans-results.md`), and
S0+ stays shipped until then. Never name a span package as R_0 in a signed
manifest. Testnet scores R0 as R_0 on a fresh chain record and may replace it
once, announced in advance, by the next qualified reference only if it passes
every gate; later reference changes follow cohort promotion. Mainnet opens a
fresh chain record with the reference testnet then holds, with no separate
one-time replacement.
Faithful word-level explanations are later A extensions; grammar-level
explanations belong to later B revision guidance.
The launch A input contract is raw text: miner artifacts are text models (ONNX
graph plus fixed tokenizer) and cohorts feed canonical text, not CFG vectors.
The manifest caps artifacts at the allow-listed ModernBERT-base graph, with or
without the token span head, and at most 160M parameters (about 700 MB follows);
the tokenizer byte-matches the allow-listed one and other files validate against
fixed role schemas, with no separate byte cap. The allow-list widens only by a
versioned manifest. Avoid arbitrary caps that can strand the subnet: derive any
needed limit from structure and version it in the manifest, never hard-wired.
A does not use CFG inputs: CFG-only inputs failed their first real-data test,
text+CFG fusion gave only a small, uncertain gain, and CFG writer vectors
scored below the A encoder and added little on top of encoder plus stylometry
(`docs/cfg-vector-viability.md`, `docs/a-v6-cfg-fusion.md`,
`docs/cfg-vs-encoder-results.md`). The grammar work continues for later B
(edit units and revision guidance, with grammar-level writer-style features a
candidate input) and for C; qualify each use on its own evidence and keep the
dated results.
Attribution returns conditional human and model results with role involvement
probabilities, without a presence cutoff. When no named profile for a role
qualifies, return `other_human` ("Other human") or `other_ai` ("Other AI").
Neither fallback asserts role presence or gallery absence; both can appear
in mixed work. A completed result has no unknown or attribution-abstention
label; execution failure remains separate. Score documented contributor roles
regardless of predicted probability; conditional results for absent roles
do not themselves assert a false attribution.
The main eventual human-attribution workflow uses a customer's own samples to
construct a private writer profile from the A encoder plus stylometric features
(function words and character n-grams), with no CFG channel, initially
proposing a normalized weighted mean under a qualified versioned pooling
contract. A learned matcher on fresh authors, with thresholds
measured on withheld writers and real style-transfer data, is the next
qualification before any profile type backs `provided_writer` or B's
author-fit feedback. A receives that profile rather
than raw reference prose and may return the named match `provided_writer`
("Provided writer"); no qualifying human match remains `other_human`. This is resemblance
to supplied references, not identity authentication or proof of authorship.
Profiles remain request/customer scoped. Documented public or licensed human
references are also allowed under an enrollment policy recording provenance and
permitted uses; do not impose a blanket writer-opt-in requirement.
Later B uses the fixed writer profile and continuous A author-fit feedback to
guide iterative revision, with separate meaning, readability, and tone checks.
Freeze profile, evaluator, and calibration within each comparison. Qualify any
A-derived mapping to the existing integer style distance/rank; that rank is not
an authorship probability. Trusted preprocessing still reads samples and editing
still needs source prose. Samples and profiles stay private, with no miner access,
automatic public enrollment, or automatic training reuse. Pooling viability and
the personal revision workflow remain future qualifications.
C credits accepted grammar contributions once. It supports the grammar later B
uses and activates by its own decision, which may precede B's; B's revised
mechanism mapping must then also place C's credit. Pilot scoring is coordinator-led.
A rewards use `reward-v1` (whitepaper Section 5), selected on
2026-10-30; its lagged-payout, fallback-assumption, unregistered-credit and
reference-failure defaults remain to be confirmed with the implementation. The baseline is the public reference model R_k, re-run on each
cohort's batch with its predecessor R_{k-1}; the top eligible entrant becomes
R_{k+1}, published after the cohort, only when it beats R_k, otherwise R_k stays.
Eligibility needs the gates and q >= delta = max(1%, 2 x pooled paired SE) of
headroom, frozen per manifest; promotion uses the same delta. Port SN78's band
mechanism faithfully: r = 0.85 across and within bands, band width w = 2 x pooled
paired SE (about 8% of headroom at 1k cases, 4% at 4k; SN78's literal 2.5 points
would be about 48%), near-equal drop epsilon = w/250, largest-remainder u16 row,
exact integer/rational arithmetic, parameters frozen in the signed manifest.
Copies earn nothing: an entrant with the content digest of any published
artifact, or within isotonic-aligned RMS <= 0.025 of R_k or R_{k-1} on the batch
or of any fingerprinted artifact on the private probe without scoring more than
w/2 above it, fails the public-copy gate. Weight soups of published models are
legitimate new models; output averages of two encoders do not fit one graph. Near-duplicate clusters (identical content, or within
the same distance on the batch) get one credit split equally. If nobody beats R_k, score against
C_N = S(R_{k-1}) + 0.15 x (max_N - S(R_{k-1})) without advancing the reference;
with no R_{k-1}, or nobody above S(R_{k-1}), keep the previous target live, or
burn to the owner UID if none exists. Unregistered credit burns; a reference
failure holds the package for a signed correction. Each cohort scores about 1k
(testnet) or 4k (mainnet, floor 2k) fresh paired cases per model, mixed 20/55/25
human_only/mixed/model_only and self-weighted by domain x origin, so the human
share sets the false-positive weight. The pure-human FPR gate is loose per
entrant on its own cohort's humans and strict for promotion on the canary's
fresh CC draw; 7-cohort pooling is monitoring only. A fresh-stock pure-human
negative is scored once and may be a rewrite source up to 4 times, uses at least
7 cohorts apart, including in its own cohort; outputs must be batch-invariant.
Testnet cohorts without an admitted outside entrant with a working artifact draw
only already-exposed documents (redrawn freely, parent limits kept); the
coordinator enforces this. Mainnet human stock is pre-cutoff CommonCrawl
(captured before 2022-11-30), a historical-human proxy with per-domain share
caps; the public stock is testnet-only. Lookup is reduced, never claimed
prevented. Two lookup-canary flags block a candidate and the next qualifying
retained cluster head is promoted; a signed operator review may later clear it without
changing past promotions.
The mainnet owner/validator/miner emission split is open; testnet 601's split is
operator configuration only. The
coordinator publishes a signed package of individual miner scores and sufficient
public components for every validator to independently derive the native row.
Held-out texts, labels, per-document predictions, customer queries, and private
model weights stay outside that package. Validators trust measured scores but
recompute reward allocation, eligibility arithmetic, normalization and quantization
under common versioned rules. Identical inputs and encoding must produce identical
canonical rows; a discrepancy pauses the affected validator before signing.
No ZK proof or independent reproduction of hidden evaluation is required.
Slop Ninja is the operator; the subnet owner is the account recognized on chain.
Ownership or a lease must provide the required owner-authorized hyperparameter
execution path, including enabling commit/reveal, and any owner-only burn path.
Service operation alone grants neither those powers nor owner emissions.
One lead validator, operated by Slop Ninja, serves as the coordinator.
The serverless service is that validator: it performs hidden evaluation,
scheduling, score publication, its own row calculation and chain signing,
finalized reconciliation, and recovery. Other validators receive no hidden
evaluation data. Keep the coordinator's hotkey, authorization, and receipt keys
distinct within the service. Its own weight delivery and certificate never gate
other validators or cohort progress.
The pilot enables native weight commit-reveal: the recognized owner sets
`commit_reveal_weights_enabled = true` through the agreed owner-authorized path;
the operator verifies it, and validators check at startup and before signing.
Validators use their own journaled timelocked commitments in parallel, without
waiting for the coordinator's chain row. Automatic reveal can occur during coordinator
silence. An unexpected disabled flag pauses new commitments rather than selecting
plaintext submission. A artifacts stay private until the cohort ends: a miner
commits only a digest on chain and uploads the artifact privately to the
coordinator's evaluation bucket, encrypted to the coordinator key where
qualified. Validators check digests only; the coordinator's signed package
publishes the scores when scoring completes and the artifacts and openings when
the cohort ends.
The coordinator's finalized row and observation certificate are later consistency
audits, never prerequisites for other validators. Missing or incorrect coordinator
rows do not invalidate a valid score package. A shared package/encoding defect
pauses dependent new work; existing chain obligations remain recoverable.
The coordinator's signed calculation target changes only when a complete
successor score package and retained receipts are available. Registration,
compatibility, authorization and receipt-retention limits remain enforceable.
Native commit-reveal hides on-chain submissions, but public scores can expose
the same derivable row; do not claim this prevents copying or proves verification.
Every inference attempt has a content-free R2 receipt bound into the coordinator's
signed score package. Validators verify its index and object digests before new
commitments. Delete cohort N's detailed receipts and index after N+1 and N+2
complete and N's target is superseded, based on durable coordinator completion
rather than tempos, wall time, or adoption. Keep the artifacts of R_k and
R_{k-1} while they are references. Preserve signed per-miner score/calculation records, roots/counts,
rows, observation certificates, and chain/accounting recovery state. New commits
need retained receipts; submitted obligations recover without expired receipts.
No indefinite detailed receipt archive.
The coordinator opens paid inference when it judges measured quality sufficient.
A cohort lasts at least K = 20 tempos of 360 blocks, about one day, frozen in the
manifest (rehearsals may use K = 4); overruns move to a later tempo boundary and
never skip. It ends with a transition of at least two tempos that issues no new
challenges. Cohort N's target activates at the first eligible tempo boundary
after its package and receipts are durable, and validators re-derive and commit
it every tempo with commit-reveal on, with no per-validator activation gate
(native validators abstain while the row is all burn; the coordinator commits it).
Reward-intent authorization is per cohort with a bounded lease of at least two
cohorts (48 h), renewed by each package and, while the coordinator is live, each
tempo without changing the target; beyond the lease validators pause new
commitments. Coordinator outages pause protocol progress indefinitely;
retain and resume the same cohort and its evidence. Never skip or expire it
because time passed, or claim that this also pauses Subtensor's native emissions.
The sole dependence on other validators is timely on-chain submission and reveal of the
independently calculated weight rows. Their acknowledgments, versions,
availability, and adopting stake never gate evaluation, transitions, service,
admissions, upgrades, or recovery. Keep publication and per-validator chain
delivery separate; publication does not prove payment. Coordinator-owned
scheduling budgets stop during coordinator silence and resume at eligible tempo
boundaries. Authorization leases and external obligations have separate clocks.
Tempo durations are minimum scheduling targets; slow work or missed wakeups
move advancement to a later boundary without discarding the cohort. Pin evaluation
semantics separately from operational builds and the live Finney runtime; runtime
upgrades never invalidate cohorts, packages, rows or journals or require
re-certification. Never bake a runtime version into any record, identity, gate,
certificate, journal or config. Permit compatible upgrades and journal chain
intents/submissions durably; do not assume an unjournaled scheduler survives
validator restarts. Never gate on a runtime spec or transaction version: the
validator and coordinator keep running through any runtime upgrade. Resolve
calls, storage, signed extensions and runtime APIs by name from live metadata and
encode adaptively where mechanically safe. Defer only an operation with no
type-correct encoding, and re-evaluate it automatically. Record spec versions
only as observations; signing reads them fresh. The native chain adapter pins
rtao main `b5553a76ff67f0f54350472ef0306f225e8a4c44` (journal schema 3,
migrating schema 1 and 2 journals on open) and uses its durable journal and
weight/reveal recovery APIs, with no spec-number refusal; rtao records no
validated runtime range, and nothing here requires one. The native validator's
live commits, per-tempo re-commits and fault drills passed on testnet subnet 601
at that pin. There rtao still composes weight calls through compiled
descriptors, reads its scheduler's storage by type and supports one
commit-reveal version; the adapter defers those cases instead of halting.
Journaled MEV-shield dispatch remains refused at that revision; never bypass
journaling to enable shielding. See `subnet/README.md`.
Coinbase Onramp is selected for card-to-native-TAO funding, then subnet alpha
burning, subject to qualifying our integration, payment methods, and settlement.
Solana is preferred for any additional crypto entry route; Ethereum mainnet is
not selected. Paid-use emissions are a separate choice.
Keep the whitepaper and design notes aligned when these decisions change.
Support Linux and macOS miner/validator tooling. Keep platform-specific ML
backends optional and declare tested OS/architecture/runtime combinations.
Production miner and validator implementations are entirely Rust using
`sam0x17/rtao`, with no Python or btcli runtime dependency. The coordinator's
Workers-compatible chain integration requires separate qualification. Use the
`sam0x17/rtao-rpc` service for RPC; it provides Slop Ninja free access. Budget
RPC service fees at zero, separately from chain fees and our runtime costs.
The coordinator is entirely serverless on Cloudflare Workers, using Durable Objects for authoritative
state, Queues for content-free lane wakeups, and R2 for artifacts and evidence.
Bulk evaluation work (generation, scoring, receipts, packaging) runs as fan-out
lanes: one Durable Object invocation per lane, claiming through the cohort
object's leases, each scoring lane driving its own on-demand Container. Persist
unfinished work independently of queue retention and process lifetime. Share
Rust/Wasm protocol logic where supported; qualify Workers chain transport
separately rather than assuming the native rtao runtime compiles to Wasm.
The coordinator holds its own validator hotkey and journals signing attempts,
nonce allocation, submissions, and reveal recovery in durable serverless state.
The native file journal does not supply that storage backend.
The rtao-rpc finalized-stream block webhook is the coordinator's primary
heartbeat and clock: it delivers every finalized block, and each delivery is a
separate Worker invocation. The webhooks are live as an unbilled Finney pilot
and a separate testnet service, each with its own subscriptions and signing
keys; receiver integration and qualification remain Slop Ninja's work.
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
Duplicate deliveries must not duplicate signing, reset budgets, or skip cohorts.
Any qualified Container fallback is for bounded computation, never coordinator
state or scheduling. Other participants retain their chain keys in native clients.
All production inference and challenge generation/execution/scoring is serverless;
no participant or development-host daemon serves those workloads. Application
logic is Rust, including coordinator and updater; generated platform bindings
are allowed. Miner/validator signed automatic updates run independently, with
ordered transactional or resumable migrations for installations missing several
releases. Preserve wallets, journals, pending work, and frozen cohort semantics;
reconcile external effects before resuming signing. No update barrier depends on
other validators, and incompatible rollback must preserve recoverable state.
Automatic updates are selected; participant opt-out/deferral policy and staggered
rollout sequencing remain to be specified before launch. Do not infer an opt-out
right or permit incompatible signing from this unresolved deployment policy.
The proposed later B service uses Cloudflare trust and miner-owned accounts.
Its privacy boundary relies on the qualified request path and account controls.

Use Rust for the CLI, corpus collection, SQLite storage, statistics, experiments,
and detector orchestration. Use Python only where an NLP/ML dependency requires
it. Current exceptions are the spaCy annotation bridges and PyTorch author
scoring experiments in `grammar/crates/grammar-eval/python/`. Keep
corpus handling, feature extraction and vector geometry in Rust.

Run CPU-heavy builds, parsing, tests, and corpus audits on the build host that
owns the checkout, not on CI. Use durable
task-owned jobs with explicit memory limits and zero swap; start with one Cargo
job and one parser worker. Reuse a build directory only for its own checkout.
Check headroom before increasing concurrency, leave unrelated processes alone,
and clean exact task-created temporary paths after checking live dependencies
and retaining bounded useful results. The repository has no CI.

Keep exact corpus text, provenance, input hashes, source-group splits, and every
detector observation. Treat source-conditioned rewrites separately from writing
generated from scratch. A low detector score does not establish fidelity,
readability, human authorship, or performance on unseen sources.

B must support long multi-paragraph documents, including book-length customer
jobs. Word, phrase, clause and sentence repairs are edit units; the native
grammar types them, and the assembled document is the evaluation unit. Test chapter and section boundaries, long-range claim
and terminology consistency, and author voice across the work. Do not infer
document success from isolated sentence or section scores. Keep all text covered,
record model context limits, and distinguish window diagnostics from calibrated
document scores. Choose an authorized execution host before running long-form experiments.

Keep generated corpora, credentials, local databases, and live invocation logs in
ignored `data/`. Check in pinned source manifests, reproducible Rust code, and
reports. The user authorized publishing the redistributable detector corpus on
2026-09-12: curated snapshots belong in `datasets/`, with exact text, provenance,
source licenses, fixed splits and checksums. Keep private material and live
provider logs in ignored storage. `legacy/` and `experiments/pangram-preface/`
preserve earlier work; do not
extend their Python orchestration for new experiments.

Published dataset JSONL, document-level rights and evidence indexes use lossless
zstd storage. Restore decoded copies with `tools/dataset-storage` before existing
readers/tests; preserve decoded-byte checksums and logical source paths. Do not
commit restored plaintext copies. Historical Git IDs in frozen receipts remain
original evidence; consult the compression migration map for their new aliases.

Private correspondence and coursework, extracted passages, identifying source
manifests, fitted profiles, and derived candidate texts stay in ignored
`data/grammar-private/`. Do not commit them or use their sentences in public
fixtures or documentation. Public examples must be synthetic or separately
authorized. Preserve provenance and group excerpts from one work together.

Reference writing from the blogging era is acceptable; there is no hard 1999
cutoff. Preserve original composition dates when available and keep source groups
separate across fitting and evaluation. Date-based exports still need usable
dates for their declared chronological split. Keep corpus discovery notes and
downloaded samples in ignored `data/author-corpora/`.

The native grammar for later B work lives in the independent `grammar/` Cargo
workspace, which also hosts parser-free A crates (`grammar-a-*`,
`grammar-cc-human`) and the public A training recipe in
`grammar/crates/grammar-eval/python/`.
Keep parsing adapters separate from Rust syntax, word counts, feature extraction,
geometry and rewrite rules. Version changes to feature meanings, denominators,
rules and geometry. Compare all candidate points in the same frozen space; a
distance improvement never certifies meaning preservation or detector success.

For native CFG campaign work, review documents in isolated agent contexts, propose reusable grammar changes,
and merge reviewed batches. Preserve exact text, informal usage, ambiguity,
and private-source restrictions. Track mechanically complete documents and
reviewed ambiguity choices separately. Keep compact parse caches and grammar
item dependencies so changed parses can be inspected and affected samples
replayed. No Docker, certification quorum, or platform replay gate belongs to
this development loop.

For Rust changes, run focused tests and relevant checks on the build host. Avoid full
workspace formatting or test sweeps for a small document batch. Broaden testing
when parser-wide changes or failures justify it. Do not issue live model or
detector calls from automated tests.

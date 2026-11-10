# Project conventions

**This repository is public.** Never commit private information: credentials,
keys, account ids, private hosts, spend figures, communication logs, private
decisions or raw transcripts. They belong in the private repository or ignored
`data/`. Never change the repository's visibility or settings, and take no
GitHub-side action (pull request, review, comment, merge, release, tag or issue
edit) without the maintainer's explicit go.

The project is named **SlopNinja**, with **slop.ninja** as the planned domain.
Use **SlopNinja**, one word, in project-facing prose and `slop_ninja` in
filenames and command names. Frozen released or hashed bytes keep their
original spelling. The GitHub repository slug is `slop-ninja`. Existing
versioned `slopninja` schemas and
cryptographic domain identifiers retain their spelling for compatibility.
Existing `unslop` command names, package names and versioned
artifact schemas remain compatibility identifiers. Describe the whitepaper and
design notes as one self-contained proposal, without references to previous or
superseded designs. Preserve dated empirical evidence and implementation limits.
Keep documentation focused on current behavior. Delete superseded plans and
experiment diaries; Git retains history. Preserve data and fixtures still used
by executable checks or current evidence.

The assets in `assets/brand/` are the approved SlopNinja logo, not a proposal.
Project code and docs are MIT OR Apache-2.0, except files and datasets with
their own licence or notice; miner submissions and reference weights are
CC BY-SA 4.0, and the `assets/brand/` logo is not covered by the code licence.
SlopNinja publishes its own references (R0 and later) like any R_k; a public
reference package carries no evaluation text (synthetic replay cases, never
Dev-iter ones) and passes a package-corpus scan. Paid A serves a published,
qualified artifact (R0 once its public package is published).
Update the whitepaper's baseline evidence at meaningful evaluated milestones,
not after every experiment. Keep both A and B's measured limits explicit.
Whitepaper draft 0.64 and `docs/cloudflare-subnet-draft.md` describe the current plan: launch A
alone; B transformation and C (shared contributions to the grammar B uses)
activate separately, each by its own decision, and C may activate before B so
the grammar can grow first. A reads raw text; the native CFG is a later B input
for edit units and revision guidance, with grammar-level writer-style features
a candidate B input qualified on B's own evidence. Present CFG in public pages
and docs as later B work, not as part of A.
Testnet (subnet 601) runs for several months before the mainnet subnet is
registered, with admission open to every registered hotkey. Testnet subnet
pages live under `/testnet/<page>`, their unprefixed paths reserved for
mainnet; the detection product, not a subnet page, lives at `/detect/`.
Revenue starts on testnet: a free AI-detection page running SlopNinja's span
package in its own runtime (no stored text, no ad or analytics script; ads
only on approved content pages), a paid detection API once the package is
published, and other products. Their payouts go into a public pot
(whitepaper Section 8): native TAO in a published two-of-three multisig with
an independent third signer, every payout, conversion and burn listed; after
mainnet registration it buys and burns that subnet's alpha in daily tranches
bound to the registration, and its TAO is burned if mainnet is abandoned. The
pot changes no miner or validator reward. Describe it operationally, never in
terms of price, value to holders or returns. The page's Worker is built, not
deployed.
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
reward; share, attribution and spans are qualification gates only. Spans are
required: from the first manifest enforcing the span gate, every entry carries
a token-level head whose decoder never emits a span under 24 words (artifact
spec v5, with a miner client release); failing the gate earns no credit. The
gate compares each entrant with a fixed anchor, SlopNinja's first span
package, on the same batch under the 24-word floor, with margins fixed after
measurement on closed testnet cohorts to let heads as good as the anchor pass; the
operator may void it for one cohort with a recorded reason. It first runs in
report mode, failing no one, on at least three live cohorts. Report mode and
spec v5 are built, not deployed: only a cohort whose template names the gate
evaluates spans (none has yet), and a document-heads-only entry like R0 is
admissible until a manifest enforces the gate. Never train or
evaluate spans under 24 words: mask shorter AI spans out of the span loss rather
than labelling them human, leave them out of span scoring, and never decode a
span under 24 words. Every AI word still counts toward document share and
involvement.
The release is R0 (seed 17) plus the S0+ token head, with document outputs
bit-identical to R0 (`docs/a-spans-results.md`). SlopNinja's first span package
keeps R0's encoder and document heads frozen, so on the same CPU class its
document outputs are bit-identical to R0's, and adds S2c's seed-17 tower and
token head with the 24-word decoder and a span confidence map fitted on the
Calibration split under the floor (whitepaper Section 3). Since November 9 it
is the span-serving package (span package d24), replacing the earlier S2c
package, whose decoder allowed shorter spans. Testnet scores R0 as R_0 on a
fresh chain record. Before the first cohort that enforces the span gate, one
signed span-equivalent reference decision passes R_0's position to d24, after
d24 runs once as an ordinary entry (published, fingerprinted); it changes no
reward input and is not the planned replacement. Later reference changes follow
cohort promotion or a signed operator reference decision with a recorded reason; one
is planned on testnet, announced in advance: replacing R_0 by the next
reference SlopNinja trains, only if it passes every gate. Mainnet opens a fresh
chain record whose first reference is the one testnet then holds, with no
predecessor; reference decisions remain available there.
Faithful word-level explanations are later A extensions; grammar-level
explanations belong to later B revision guidance.
The launch A input contract is raw text: miner artifacts are text models (ONNX
graph plus fixed tokenizer) and cohorts feed canonical text, not CFG vectors.
The manifest's allow-list names three graph classes: R0's ModernBERT-base
architecture; the same architecture exposing its final-norm token states to a
token-level span head; and a tower class adding trained copies of encoder
layers 18-21 and the final norm for that head. Each class has a parameter bound
derived from its structure: 160M for the first two and 173.5M for the tower
class; R0's package has 150.3M and the first span package 170.6M. The tokenizer
byte-matches the allow-listed one and other files validate against fixed role
schemas, with no separate byte cap.
The allow-list widens only by a versioned manifest. Avoid arbitrary caps that
can strand the subnet: derive any needed limit from structure and version it in
the manifest, never hard-wired.
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
A rewards use `reward-v2`: the `reward-v1` band rule selected on
2026-10-30 (whitepaper Section 5), computed from quantized scores under
`quant-v1` (approved 2026-11-04), with its lagged-payout, fallback-assumption,
unregistered-credit and reference-failure defaults as built (confirmed
2026-11-09). Testnet's record restarted from R_0 under `reward-v2` and
`quant-v1` on 2026-11-06. The baseline is the public reference model R_k, re-run on each
cohort's batch with its predecessor R_{k-1}. An entrant beats R_k when it passes
the gates and its quantized score reaches a margin cutoff frozen per manifest;
the first retained cluster head that also passes every promotion check becomes
R_{k+1}. Credit follows SN78's band mechanism with exact integer/rational
arithmetic. Exact scores stay inside the coordinator; every released score is
quantized and noised under a versioned privacy and bit budget. Copies earn
nothing. [`docs/agents/rewards.md`](docs/agents/rewards.md) holds the full
rules (cutoff, promotion, bands, quantization, bit budget, public-copy gate,
fallback target, cohort size and mix, human stock, lookup canary); change them
only together with the whitepaper and the signed manifest.
Half of each cohort's generated text is to come from frontier models, to which
the coordinator has access through configured credentials, and half from open
models on Workers AI; until the frontier half is enabled, testnet batches are
generated by open models. Public text names no endpoint, account or route
behind that access.
The mainnet owner/validator/miner emission split is open; testnet 601's split is
operator configuration only. The coordinator alone measures entrants, computes
the reward outcome and derives the native row. It signs a private score package
(exact scores, every hidden measurement, the quantization draws, the receipt
root and count), which never leaves its storage, and a public quantized result
from which anyone can recompute the row. Once a tempo it signs a coordinator row
record when the paid entries change and publishes it off-chain before its own
hotkey signs anything. Every other validator verifies the record under its
pinned keys and commits its entries through its own journaled commit-reveal; it
needs nothing else to commit. [`docs/agents/coordinator.md`](docs/agents/coordinator.md)
gives the result's and record's contents, the validator's checks, artifact
intake, receipts, cohort timing and the webhook path in full.
Validators and the public never learn held-out data: texts, labels,
per-document predictions, exact scores, receipts, customer queries and private
model weights stay coordinator-private. A validator may recompute the row from
the public result, check models in its own way and publish its own scores;
those checks are advisory and never change its row.
No ZK proof or independent reproduction of hidden evaluation is required.
SlopNinja is the operator; the subnet owner is the account recognized on chain.
Ownership or a lease must provide the required owner-authorized hyperparameter
execution path, including enabling commit/reveal, and any owner-only burn path.
Service operation alone grants neither those powers nor owner emissions.
One lead validator, operated by SlopNinja, serves as the coordinator.
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
plaintext submission. A artifacts stay private until their cohort's scoring
starts: a miner commits only a digest on chain and uploads each file straight
to the coordinator's private R2 admission bucket with presigned PUTs that R2
verifies on arrival; no upload passes through a SlopNinja Worker, and nothing
hashes model bytes again. Every admitted artifact is published when its
cohort's scoring starts and is withdrawn only by a recorded legal removal
(a content-free public record of the removal and its legal basis), which
changes no score, row or payment. After the seal, each executed
model's quantized score (or withheld outcome) is shown as pre-finalized once
final, and testnet cohorts may release a few `live-v1` segment readings under
their own privacy budget, with an operator stop (both built, not deployed); no
rank, band, cutoff mode, gate, public-copy, cluster or unit value appears before
the close, when the remaining allow-listed values and the openings are
published. Validators need none of them.
The coordinator's finalized row and observation certificate are later consistency
audits, never prerequisites for other validators. A missing or incorrect
coordinator chain row does not invalidate a valid signed row record. A shared
row/encoding defect pauses dependent new work; existing chain obligations remain
recoverable.
The coordinator's signed calculation target changes only when a complete
successor score package and retained receipts are available. Registration,
compatibility, authorization and receipt-retention limits remain enforceable.
Native commit-reveal hides on-chain submissions, but the coordinator's row is
published off-chain as soon as it is signed, before any reveal, and follows from
the public quantized result; do not claim this prevents copying or proves verification.
Every inference attempt on cohort text has a content-free R2 receipt bound into
the coordinator's signed score package: coordinator-private audit evidence,
never published or sent to validators. Outage probes, operator pre-flight probes
and admission runs on fixed control text are the only receipt-less calls. The
coordinator installs a new target only when that cohort's receipt index
verifies. Delete cohort N's detailed receipts and index after N+1 and N+2
complete and N's target is superseded, based on durable coordinator completion
rather than tempos, wall time, or adoption. Validators' commitments need only
the signed row; submitted obligations recover without expired receipts. No
indefinite detailed receipt archive.
The coordinator opens paid inference when it judges measured quality sufficient.
A cohort lasts at least K = 20 tempos of 360 blocks, about one day, frozen in the
manifest; overruns move to a later tempo boundary and never skip. It ends with a
transition of at least two tempos that issues no new challenges. Each activated
target's signed row is committed by every validator every tempo with
commit-reveal on, with no per-validator activation gate; a validator commits
the owner burn row when the chain's limits allow and otherwise defers that
tempo. Reward-intent authorization is per
cohort and never expires: it stays valid until it is superseded (another target
activates) or a signed revocation withdraws it, so validators keep committing
the last signed row through coordinator silence. A revocation entry that no
longer decodes is treated as in force until the operator restores or removes
it. Coordinator outages pause protocol progress indefinitely;
retain and resume the same cohort and its evidence. Never skip or expire it
because time passed, or claim that this also pauses Subtensor's native emissions.
Never let our own rules lock the operator out. Every precondition in the
evaluation, coordinator, publisher, validator and deploy tooling needs a
reasonable, journaled operator path past it: the operator can always decide to
start a new cohort, change references, install or pause a template, or restart
the record without first writing code. Moving forward never requires reading
retired or superseded state, and an install respects a paused calendar. Escape
paths keep the never-skip, no-expiry and no-double-sign rules and are recorded.
The operator decisions (whitepaper Section 6) abandon an unclosed cohort,
refuse to pay a finished one, start over at R_0 through a switch to burn,
change the reward rule only through such a fresh record, set references with a
recorded reason, and rotate the authorization key on re-registration. Each is
journaled and publishes a signed, content-free record of its reason; those that
decline a result, change reward inputs or restart the record also need a
separate offline operator-decision key, verified only by the coordinator and the
evaluation service, never by validators. They stay available on mainnet, no
outage triggers one, and none reassigns paid rewards. Most paths and the
operator-decision key's verification are built, not deployed;
`docs/agents/coordinator.md` lists each path in full and what is not built yet.
The sole dependence on other validators is timely on-chain submission and reveal of the
coordinator's signed weight rows. Their acknowledgments, versions,
availability, and adopting stake never gate evaluation, transitions, service,
admissions, upgrades, or recovery. Keep publication and per-validator chain
delivery separate; publication does not prove payment. Coordinator-owned
scheduling budgets stop during coordinator silence and resume at eligible tempo
boundaries. Coordinator work leases and external obligations have separate
clocks; validators' reward-intent authorizations never expire.
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
only as observations; signing reads them fresh. Watch the testnet and Finney
runtime versions and qualify the stack against each change; that work never
blocks or gates anything. The native chain adapter pins
rtao `fa83052d37b39f002f7a4d3cc5c04f25503e1d11` (immutable tag
`snapshot-2026-11-05.2`; pin the full commit, never a branch) in every crate
that uses rtao, with its durable journal (schema 3) and weight/reveal recovery
APIs and no spec-number refusal; nothing here requires a validated runtime
range. The adapter submits unshielded and never bypasses journaling to enable
shielding. Before rolling back to an earlier pin, stop the validator and run its
`rollback-check`. [`docs/agents/chain-adapter.md`](docs/agents/chain-adapter.md)
has the pin's details, rollback limits and live evidence; see also
`subnet/README.md`.
For paid jobs, Coinbase Onramp is selected for card-to-native-TAO funding, then
subnet alpha burning, subject to qualifying our integration, payment methods,
and settlement; the detection API uses a merchant-of-record card processor or
direct TAO and settles through the pot.
Solana is preferred for any additional crypto entry route; Ethereum mainnet is
not selected. Paid-use emissions are a separate choice.
Keep the whitepaper and design notes aligned when these decisions change.
Support Linux and macOS miner/validator tooling. Keep platform-specific ML
backends optional and declare tested OS/architecture/runtime combinations.
Production miner and validator implementations are entirely Rust using
`sam0x17/rtao`, with no Python or btcli runtime dependency. The coordinator's
Workers-compatible chain integration requires separate qualification. Use the
`sam0x17/rtao-rpc` service for RPC; it provides SlopNinja free access. Budget
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
heartbeat and clock: each finalized block is a separate Worker invocation,
authenticated and persisted before acknowledgment, deduplicated and applied to
a finalized block height/hash cursor, with any gap backfilled through bounded
RPC. Notifications alone establish neither finality nor authorization. A
one-minute cron watchdog is the only fallback: after five minutes
(configurable) without valid deliveries, or with the cursor lagging by that
threshold, it polls the public OpenTensor RPC through the same per-block path
until webhooks resume. Qualify measured lag before launch.
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
The signed updater is planned and not yet built; until it exists, participants
update by hand.
The proposed later B service uses Cloudflare trust and miner-owned accounts.
Its privacy boundary relies on the qualified request path and account controls.

Use Rust for the CLI, corpus collection, SQLite storage, statistics, experiments,
and detector orchestration. Use Python only where an NLP/ML dependency requires
it. Current exceptions are the spaCy and transformer bridges called
by Rust; the A training, export, final-block, spans and Tier C recipes and their tests in
`grammar/crates/grammar-eval/python/`; the V6 detector runner in
`baseline/detector/ml/`; and the A CPU qualification oracle. Delete Python that
no current check, recipe or cited evidence uses. Keep corpus handling, feature
extraction and vector geometry in Rust.

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
provider logs in ignored storage. Git keeps the history of removed prototypes
and experiments; remaining Python is limited to the exceptions listed above.

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

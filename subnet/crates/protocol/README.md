# SlopNinja subnet protocol

`slop-ninja-protocol` holds the protocol logic shared by the native validator
and the Workers coordinator: versioned records, canonical encoding,
domain-separated digests and signatures, stable operation identities, private
submission records, artifact objects and their inference-content identity, the
validators' launch and reward-intent authorizations, the published-artifact
index and near-copy relation of copy policy B + D, the verification of an
active-target bundle, the `quant-v1` score quantization and its public
`QuantizedResult`, and two row calculations: `reward-v2`, the band rule of
whitepaper Section 5 computed from quantized scores only, and the
testnet-only `testnet-reward-v0`. It is
pure computation, with no I/O, clock, randomness or chain access, and it builds
for native targets and `wasm32-unknown-unknown`. Chain submission stays in the
chain adapters. No record, identity or check carries a chain runtime version.

## Records

Each record encodes as `str(kind) || u16(version) || fields`. Its digest is
`SHA-256(u32_le(len(D)) || D || bytes)` with `D = slop_ninja.protocol/<kind>/v<version>`.
Encoding and decoding both validate, so every accepted record meets its
invariants and has exactly one encoding. Version-1 records carry
`testnet-reward-v0` only and the `reward-v1` records (version 2 and later)
carry `reward-v1` only; each
family refuses the other rule (`mismatch: reward rule record version`), so
neither calculation can read the other's inputs.

| Record (kind, v1) | Purpose | Signed by |
| --- | --- | --- |
| `cohort-manifest` | Frozen comparison: chain binding, cohort number and label, opening block, evaluation-set digest, task and group counts, baseline artifact, reward rule, attempt limit, allow-list, burn identity and admitted artifacts. | package key |
| `artifact` | Miner artifact: owner identity, contract, package, model and image digests, commitment block. Its digest is the artifact ID. | none |
| `assignment` | One artifact on one private task index and group. | none |
| `attempt` | Numbered execution of an assignment. Its digest is the stable attempt ID. | none |
| `result` | Private outcome: prediction (involvement and share, share at most involvement) or a model, output, parse or infrastructure failure. Never published. | none |
| `attempt-receipt` | Content-free object per attempt: IDs, outcome class and a salted result commitment. | none (bound by the index) |
| `receipt-index` | Every receipt's attempt ID, outcome class, object SHA-256 and length, sorted. Its digest is the receipt root; its length is the receipt count. | receipt key |
| `score-package` | Public per-miner score components, baseline, allow-list, burn identity, receipt root and count. | package key |
| `calculation-target` | Package digest, receipt root and count, revision and predecessor. | package key |
| `registration-snapshot` | Each validator's own finalized `(uid, hotkey, registration_block)` set. | none |
| `weight-row` | Canonical `u16` row with its target and snapshot digests. Validators compare rows by digest. | none |

| Record (kind, version) | Purpose | Signed by |
| --- | --- | --- |
| `cohort-manifest` v4 | Frozen `reward-v1` comparison, sealed with its roster and batch: chain, cohort and label, opening epoch, schedule (`K`, admission and transition minimums, the reward-intent renew-by hint distance of at least `2K` tempos, field `lease_tempos`), batch design (`n`, source groups, domain x origin strata, the origin mix, sealed batch digest), parameters (including `d` and the `w / 2` clause), references `R_k` and `R_k-1`, the published-artifact index binding, the fingerprint probe versions in use, input profile, output contract, gallery, gate policy, span-gate flag, limits, attempt limit, roster cap, and the admitted commitments in admission order. | package key |
| `score-package` v4 | Per-entrant components (identity, commitment and position, content and artifact digests, gate failures with reason codes, exact score, accounting, public-copy statistics of every scored entrant against both references on the batch and on every probe version in use, cluster head and the statistics of a distance join, units), both reference scores, parameters, the index binding and probe versions, cutoff mode, promotion and the promotion audits behind it, each retained cluster's exact credit, the distance monitor, receipt root and count. | package key |
| `canary-clearance` v1 | A signed operator review that clears a candidate the lookup canary blocked: chain, cohort, manifest and package digests, the blocked head, the private review record's digest and the clearing epoch. It has no reward effect. | package key |
| `calculation-target` v2 | The launch burn row, or a cohort package that produced a target, with its predecessor. It names no owner hotkey. | package key |
| `cohort-closure` v1 | The epochs at which admission closed, publication completed and the cohort closed (where its target activates), and every entrant's commitment opening. | package key |
| `registration-snapshot` v2 | A validator's finalized registrations and finalized subnet owner hotkey. | none |
| `weight-row` v2 | Canonical `reward-v1` row: units by UID, summing to 65,535, and the snapshot's owner hotkey, whose UID received any burned units. | none |
| `submission-upload` v1 | Private upload envelope: the opening, the on-chain digest it opens, and the exact artifact object's SHA-256 and length. | none |
| `inference-content` v1 | Canonical inference-content identity, derived from an artifact header; its digest is the content digest. | none |
| `artifact-header` v1 | File table at the start of an artifact object: contract, and each file's name, role, SHA-256 and length, sorted by name. | none |
| `reward-intent-authorization` v2 | Per-cohort authorization of the target active after a package (new, or carried over when the cohort produced none): chain, cohort, manifest, the package that carries it, target, checkpoint generation, issue epoch and a non-binding renew-by hint (no expiry). | authorization key |
| `launch-authorization` v1 | Manifest-free authorization of the launch burn row: chain, target (the launch burn digest), checkpoint generation, issue epoch and a renew-by hint 40 tempos later (no expiry). | authorization key |
| `reward-intent-renewal` v1 | Refresh of one authorization's renew-by hint, per-cohort or launch: its digest, checkpoint generation, issue epoch and hint at the same distance. Never required. | authorization key |
| `reward-intent-revocation` v1 | Signed withdrawal of a target: chain, target, the authorization last issued for it, checkpoint generation and issue epoch; withdraws every authorization of the target issued at or before that epoch. | authorization key |
| `published-contents` v1 | The sorted inference-content digests of every published artifact; its digest is the content root. | none |
| `fingerprint-index` v1 | Each fingerprinted artifact's content, probe version and private fingerprint digest, sorted, one per content and version; its digest is the fingerprint root. | none |
| `EntrantFingerprint` (closure input) | One executed entrant's probe fingerprint digest on one probe version, by hotkey; `PublishedIndex::after_closure` keeps the earliest-committed entrant's for each content and version. | none |

Integers are fixed-width little-endian. Strings and sequences carry a `u32`
length. Option and enum tags are one byte. An exact credit's numerator and
denominator are big unsigned integers: a `u32` limb count, then little-endian
`u32` limbs, least significant first, with no most-significant zero limb and at
most 128 limbs. Decoders reject trailing bytes,
truncation, unknown tags, non-0/1 booleans, invalid UTF-8, out-of-range values,
unsorted or duplicated sorted sets, and lengths above the limits. A sequence
decoder reserves at most 64 KiB (`MAX_PREALLOC_BYTES`) before its items decode,
so an untrusted item count cannot force an allocation larger than the input.
`ScorePackage::check_receipts`, `ScorePackageV2::check_receipts` and
`ReceiptIndex::verify_object` bind the retained index and each stored object to
the package.

Signatures are Ed25519 (`verify_strict`) over
`str("slop_ninja.protocol.signature/v1") || u8(role) || str(record domain) || record digest`.
`TrustedKeys` holds one public key per role (package, receipt, authorization)
and rejects a set in which two roles share a key. A record signed under the
wrong role or by an untrusted key is rejected. `Signed::open` checks the key and
signature over the raw record bytes before decoding them, so unauthenticated
bytes are never decoded; canonical decoding makes that digest equal to the
decoded record's. These application keys are
separate from chain hotkeys. The authorization key signs reward-intent
authorizations, their hint refreshes and revocations.

`weight_operation_id` reproduces the native adapter's existing journal identity
(`slopninja:weights:v1:...`, a retained compatibility identifier), and the
adapter now calls it. `WeightOperation::for_target` derives that identity from a
calculation target and validator hotkey. The netuid always comes from
configuration.

`weight_tempo_operation_id(base, epoch)` is the identity of a per-tempo
re-commit of that target (plan decision 12): `slop_ninja:weights:tempo:v1:`
followed by the hex `domain_hash("slop_ninja.protocol/weight-tempo-operation/v1",
u64_be(len(base)) || base || u64_be(epoch))`, where `base` is the target's
`weight_operation_id` and `epoch` the `SubnetEpochIndex` the re-commit is
scheduled for. It accepts only a base identity, never a tempo identity. The
golden report pins it as `operation.tempo`.
`WeightOperationV2` is the per-tempo `reward-v1` identity:
`slop_ninja:weights:v2:` followed by the hex digest (domain
`slop_ninja.protocol/weight-operation/v2`) of the genesis, netuid, mechanism,
validator hotkey, active target digest and the chain's epoch index. Each
tempo's re-commit of one target is a distinct operation, and the row is
excluded, so a changed row under the same identity conflicts.
`WeightOperationV2::for_target` computes the identity only; reconciling an
already-submitted commitment uses it without an authorization. A new commitment goes
through `CheckedAuthorization::operation`, described below.

## Reward-intent authorization

Two records authorize new weight commitments, both signed under the
authorization key, and both are checked from their signed envelope
(`Signed<_>`) against `TrustedKeys`, so neither is used without its signature.

- `LaunchAuthorization` names only the chain and the launch burn row, with no
  manifest, so validators commit the burn row from the first tempo of the
  first cohort. Its renew-by hint is `issued_epoch +
  LAUNCH_RENEW_BY_TEMPOS` (40). `LaunchAuthorization::check(signed, keys,
  chain, active)` requires the validator's chain, the launch burn digest of
  that chain, that hint distance (a format check), and an active target that
  is still the launch burn row. Once any cohort target
  has activated every validator refuses it (`launch authorization after
  activation`), at its check and at every per-tempo operation, so the burn row
  is never re-authorized after a cohort target has been active. While the
  first cohort's new target is pending (its package published, its closure not
  yet reached) the burn row is still active and the launch authorization keeps
  serving. This is deliberate: between the first package's publication and the
  closing boundary (tempos 16 to 20 at nominal phases) no other record
  authorizes the burn row, so a cut at the first package would leave
  validators without a commitment for at least two tempos, and the launch
  authorization can only ever commit the burn row, which pays no miner.
- Each package is published with a per-cohort `RewardIntentAuthorization` for
  the target active after it. Its renew-by hint is `issued_epoch` plus the
  manifest's frozen `lease_tempos` (at least `2K`; the field keeps its name in
  the frozen manifest format). `RewardIntentAuthorization::check(signed, keys,
  manifest, package, target, active)` requires the chain, cohort, manifest and
  package digests, that hint distance, an issue epoch at or after the cohort's
  opening,
  and the named target. A package that produced a target authorizes exactly
  that target, whose predecessor must be the validator's active target (the
  first cohort target's predecessor is the launch burn digest), or which is
  itself active once its closure activated it. A package that produced none
  authorizes the active target, which is the launch burn row only while no
  cohort target has ever activated, and never a target of its own or a later
  cohort.

`ActiveTarget` is the validator's active target, built only as
`ActiveTarget::launch(chain)` or `ActiveTarget::activated(checked_closure)`
from the latest checked closure that produced a target, with that closure's
closing epoch. A validator with a gap in its history holds a stale active
target, so the checks above fail and new commitments pause until it fetches
the missing package and closure.

**No authorization expires.** An authorization yields commitments from its
issue epoch for as long as its target is the validator's active target, with
no end epoch. It stops only when a newer authorization supersedes it (another
target activates) or a signed revocation withdraws it, so a silent
coordinator never stops validators committing the last authorized target.
The renew-by hint is part of each record's format and never refuses or
pauses anything.

Both checks return a `CheckedAuthorization`. A `RewardIntentRenewal` names the
authorization whose hint it refreshes (per-cohort or launch), so it cannot
change the target, keeps the hint distance and never has an earlier issue
epoch or checkpoint generation. `RewardIntentRenewal::check(signed, keys,
&CheckedAuthorization)` returns a `CheckedRenewal`. Nothing requires one, and
the coordinator issues none.

A `RewardIntentRevocation` names the chain and a target and withdraws every
authorization of that target issued at or before its own `issued_epoch`; an
authorization of the same target issued later re-authorizes it.
`RewardIntentRevocation::new(&CheckedAuthorization, generation, epoch)`
refuses an epoch before the authorization's issue.
`RewardIntentRevocation::check(signed, keys, chain)` verifies the signature
under the authorization key and the chain and returns a `CheckedRevocation`;
`CheckedRevocation::withdraws(&CheckedAuthorization)` applies the rule.
Holders journal every checked revocation and pass them to each operation as
`RewardIntentRevocation::journaled`, which does not verify the signature
again: a revocation can only withdraw, and only authorizations of its own
chain and target, so a trust root rotated since one was journaled never
turns it into an error that stops other targets. Whoever signs or admits a
revocation bounds its issue epoch by the finalized epoch index plus one
(`reward_v1_verify revoke --finalized-epoch`, the coordinator's
`POST /signing/revoke`): a re-authorization must be issued later than the
revocation and cannot commit before its own issue, so a far-future epoch
would withdraw the target that long. Recovery from one that was journaled
anyway is a re-authorization issued after its epoch (committing from that
epoch) or, for a cohort target, the next cohort's activation.

`CheckedAuthorization::operation(renewal, revocations, active, hotkey, epoch)` is the only
path to a new per-tempo commitment. Neither authorization record has an
`operation` of its own, and only the two `check` functions build a
`CheckedAuthorization`; the type's doctests show that neither compiles
otherwise. With the epoch index from a fresh finalized view and the
`ActiveTarget` the validator holds at that tempo, the operation re-checks the
active target every time: a launch authorization is refused once any cohort
target has activated (`launch authorization after activation`), so one checked
before the first closing boundary and kept across it commits the burn row no
longer; the authorized target must be the active one (`authorized target not
active`), so a cohort authorization stops once a later cohort's target
activates and a pending target is never committed early; the epoch must be at
or after the target's activation (the closing epoch of the `CheckedClosure`
that activated it; the launch burn row has none); the epoch must be at or
after the authorization's issue (`epoch before authorization`); and no
checked revocation may withdraw it (`revoked`). There is no end epoch. A
`CheckedClosure` exists only as the result of `CohortClosure::check`, so a
target never activates through a closure whose phase epochs have not been
checked against the manifest's frozen minimums. Commitments already submitted
keep their recorded authority and native deadlines. No target names the owner hotkey, so an owner-hotkey rotation never
invalidates an authorization or a carried-over target.

## Private submission

A miner commits on chain only a digest: `SubmissionOpening::commitment()`, the
domain hash (`slop_ninja.protocol/submission-commitment/v1`) of the chain,
cohort, hotkey, registration block, canonical inference-content digest and a
fresh secret 256-bit nonce. The commitment call carries the 44 bytes
`"sna1" || u64_le(cohort) || digest` (`chain_payload`). Before the admission
cutoff the miner uploads the artifact object privately in a `SubmissionUpload`
envelope, which must open that digest. `admit` takes the admitting cohort and
the `FinalizedCommitment` the coordinator read from finalized state: the chain,
the committing hotkey's registration (UID, hotkey and registration block) at
the commitment block, that block, the extrinsic index and the committed bytes.
It requires the opening to name that cohort and chain, the committing hotkey
and registration generation to be the opening's, the hotkey to have been
registered no later than the commitment block, the envelope to open the payload
and carry exactly the verified object, and the object's derived inference
content to be the committed one. The chain, committer and payload checks are
`SubmissionOpening::check_finalized`. It returns the `RosterEntry` at the
commitment's finalized block and extrinsic index, which set admission order.
Whether the commitment finalized before the admission cutoff is the
coordinator's scheduling check. After the cohort closes, the signed
`CohortClosure` publishes every nonce. `CohortClosure::check` rebuilds each
opening from the package, requires it to open the entrant's commitment, and
returns a `CheckedClosure`. `CheckedClosure::check_entrant` takes the
`FinalizedCommitment` the validator reads from its own finalized view and
requires `check_finalized` to return exactly the package's entrant: the same
committing registration, UID included, and the same block and extrinsic index,
the position that breaks rank-order ties. It also checks the published object
against the package: its SHA-256 is the package's artifact digest and its
derived content is the committed content (`check_published_artifact`;
`ReferenceModel::check_artifact` does the same for hosted references). That
digest is also the public result's `ResultEntrant::artifact`, which is what
the publisher, never reading the private package, checks a published header
against. The commitment binds the content digest rather than the
object's bytes, so the published object reaches its on-chain commitment through
its derived content. Encryption to the coordinator key is a later, separately
qualified layer around the upload bytes.

An artifact object is `u32_le(len(H)) || H || files`: `H` is the canonical
`artifact-header` (at most 1 MiB), and the files follow in its name order with
nothing after. Names are `/`-separated segments of `[A-Za-z0-9_.-]`, never `.`
or `..`; files are nonempty. Roles are graph, weights, tokenizer, calibration
and output schema, which are inference components, and reference vectors,
owner metadata and publisher signature, which are not. `ArtifactReader`
streams an object in chunks of any size, checks each file's SHA-256, and yields
a `VerifiedArtifact` with the object's SHA-256 and length and the derived
content digest; `artifact_object` assembles one.

The closure also records three finalized epoch indices: `admission_closed_epoch`
(the boundary that froze the roster), `published_epoch` (the boundary at which
publication completed and the transition began) and `closed_epoch`. `check`
requires admission to have lasted at least the manifest's `admission_min`,
publication to follow admission, the transition to have lasted at least
`transition_min` (`closed_epoch >= published_epoch + transition_min`), and the
cohort at least `K` tempos. A package cannot carry its own publication
boundary, since it is sealed before that boundary, so these epochs belong to the
closure.

Content identity (`InferenceContent`) is the frozen contract plus the sorted
set of inference components by role, SHA-256 and length, derived from the
header (`ArtifactHeader::content`). File names, publisher signatures, owner
metadata, reference test vectors and the commitment nonce are excluded, so
renaming or re-signing an artifact cannot create new inference content. Two
inference files with the same role, digest and length are refused.

## `quant-v1`

Only quantized scores leave the coordinator (`src/quant.rs`,
`src/quantized_result.rs`). For each executed entrant and for `R_k` and
`R_k-1` the coordinator publishes `q = w floor((S + eta + o) / w)`, clamped
to `[0, w floor(1 / w)]`, the largest grid value at most 1:

- `w` is `max(2 SE, w_floor)` rounded up to one significant decimal digit
  (`ladder`), so every multiple of `w` prints exactly. `SE` is the sampling
  standard error of `S_Rk` on the batch, from `S1 = sum u` and
  `S2 = sum u^2` in 256-bit integers with every rounding up (`SeMeasure`).
  `w_floor = ceil(tau)` is the noise scale: the approved floor `c/n` with
  `c = m / eps_total`, so the grid is never finer than the noise. The
  floor's `c` and the one-digit ladder are part of `quant-v1`.
- `eta` is exact discrete-Laplace noise of scale
  `tau = ceil(10^18 / n) m / eps_total` (Canonne, Kamath and Steinke 2020,
  integer-only: `bern_exp`, `dlap`); `eps_total` (thousandths, a manifest
  parameter) is split equally over the `m` released values.
- `o` is a uniform offset in `[0, w)`.
- Both are drawn from a stream keyed by the private cohort seed, the
  cohort's manifest digest (`CohortBinding`) and the subject's inference
  content (an entrant's or a reference's content digest, domain-separated
  by role), so the coordinator reproduces every draw
  (`reward_v1::check_draws`), no draw depends on another subject or on
  order, copies of one model under several hotkeys share one draw and one
  `q`, and no two cohorts share a draw even under a reused label or seed.

The published bit budget counts every deterministic hidden-data value a
result releases (per entrant `q` or withheld, band, gate bit and copy bit;
the references; cluster membership; the promotion; `w`) and must satisfy
`total * F <= n h` (`QuantParams`). The manifest checks the worst case over
every roster size up to its cap and both reference shapes (with and without
`R_k-1`: without it `m` is smaller and the grid can be a ladder step finer),
so a manifest that passes is never refused at seal; the package and the
result check the actual count.

The minimum-success gate (`Gate::MinimumSuccess`) withholds an entrant with
more than `k_n` attributable failures: its public outcome is `Withheld`.

`QuantizedResult` (kind `quantized-result`, version 1, package key) is the
cohort's public record: per entrant only its outcome (`q` with its gate and
copy bits and cluster membership, `Withheld`, or `NotExecuted`), band rank
and units, plus the grid, the cutoff, the bands, the references' `q`, the
promotion and the budget. Its validation recomputes the cutoff, bands, band
ranks, units and budget from those values alone (`reward_v1::derive_units`).
The score package (version 5) holds the exact values, the SE sums and the
draws, and names its result's digest; the closure (version 2), the target
(version 3) and `CoordinatorRow` (version 2) name it too.

## `reward-v2`

The rule ports subnet 78's band mechanism with the paper's unit translation,
and computes everything that orders or pays from the quantized scores `q`
alone. Scores are utilities scaled by 10^18. Every comparison, band and
credit is exact: checked `u128`, and the crate's own `BigUint` and `Fraction`
for geometric credits, whose denominators exceed `u128`. It replaced
`reward-v1` in place: rank ties broken by roster order, the near-equal drop
removed, ties on `q` sharing credit, and leftover units by largest remainder
with equal remainders in roster order (tied entrants have equal remainders).

0. **Parameters.** The manifest freezes `sigma`, the pooled paired standard
   error of `q`, and the rule's constants, which must equal their frozen
   values: `delta = max(0.01, 2 sigma)`, `W = 2 sigma (1 - q_Rk)`,
   `r = 0.85`, the 15% fallback fraction, the near-copy
   distance `d = 0.025` (`NEAR_COPY_PPB`), the `w / 2` score clause
   (`IMPROVEMENT_DIVISOR`) and 65,535 units. On Finney the batch holds at least
   2,000 cases. The origin mix is a manifest parameter: whole percentages that
   sum to 100 with every origin present (`OriginMix::validate`); the launch
   mix is 20% human-only, 55% mixed and 25% model-only (`LAUNCH_ORIGIN_MIX`).
1. **Scores.** `u = 10^18 - (a - 10^9 y)^2` per document. An attributable
   failure scores 0 and keeps its weight. The batch is self-weighting, so
   `S = floor(sum u / n)`. A reference with any failed prediction holds the
   package: `reference_score` refuses it, and a `ReferenceScore` with a failure
   is invalid.
2. **Gates.** An entrant is gated when it was scored and no enforced gate
   (qualification, the public-copy gate, share, attribution, the loose tier of
   the pure-human false-positive gate, and span when the manifest enforces it)
   failed. Failures carry reason codes and earn nothing. The false-positive
   gate (`Gate::HumanFalsePositive`, reason `human_fpr.loose`) fails an
   entrant whose share of its cohort's human-only documents at or above the
   frozen operating threshold exceeds the loose limit; both live in the gate
   policy the manifest binds.
   The public-copy gate is described below; it runs before the cutoff, so a
   copy cannot set `max_N`, trigger the fallback or be promoted.
3. **Rank order.** `q` descending, then roster index ascending. No exact
   score enters it.
4. **Cutoff.** All thresholds are rounded up onto the grid. If a gated
   entrant's `q` reaches `T_q = w ceil((q_Rk + delta (1 - q_Rk)) / w)`, the
   mode is `Margin`. The first cluster head in q-rank order whose exact score
   meets the exact margin over `S_Rk` and that passes every promotion
   condition becomes `R_k+1`
   (`next_references`); when none passes, nobody is promoted, the reference
   stays and the credits are unchanged. The conditions are the strict tier of
   the false-positive gate, measured on the first canary draw, and the lookup
   canary: a first flag is followed at once by a re-audit on a fresh,
   independent draw, and two flags block the candidate. The package records a
   `PromotionAudit` for each head walked, in rank order, up to and including
   the promoted one (or all retained heads when none passes); `check_outcome`
   requires exactly that walk. The audits are coordinator measurements on hidden predictions,
   trusted like the scores; only a signed `canary-clearance` review clears a
   blocked candidate, and it changes no credit and no past promotion. Otherwise, when `R_k-1` exists and a
   gated entrant's `q` is strictly above `q_Rk-1`, the mode is `Fallback` with
   `C_q = w ceil((q_Rk-1 + 0.15 (max_q - q_Rk-1)) / w)`, and the reference
   stays. Otherwise, or when `q_Rk = 1`, the mode is `NoTarget`: the previous target stays
   active, or `CalculationTargetV2::launch_burn` if none was ever produced.
5. **Clusters.** Walking gated entrants at or above the cutoff in rank order,
   the first unassigned entrant heads a cluster with every later unassigned
   entrant of identical inference content or within the near-copy distance `d`
   of the head on the batch, with no chaining (`assign_clusters`). In rank
   order a member never outscores its head, so the distance alone decides. The
   package publishes each entrant's head and, for each member joined by
   distance, its `NearStats` against the head. Validators trust those
   measurements like the scores, and the absence of a join, but check the
   structure (`cluster_heads`): exactly the gated entrants at or above the
   cutoff have heads, a head ranks first in its cluster and points to itself,
   identical content never heads a second cluster or joins a cluster later
   than its first head's, a member with identical content carries no
   statistics, and every other member's statistics are within `d`.
6. **No near-equal drop.** Two distinct `q` differ by at least `w`, far above
   reward-v1's `W / 250`, so the drop could only fire on exact ties, which
   step 8 shares.
7. **Bands.** `W_q = w max(1, ceil(W / w))` and
   `b = min(floor(q / W_q), ceil(1 / W_q) - 1)`. Occupied bands rank `kappa`
   from the top, and clusters `mu` within a band in head q-rank order.
8. **Credit and split.** `c = r^kappa / sum_{j<B} r^j * r^mu / sum_{j<n} r^j`,
   summing to exactly one; each of a cluster's `m` members receives `c / m`.
   With `r = 17/20` in lowest terms, each `c` comes out in lowest terms: its
   numerator is a product of powers of 17 and 20, and its denominator
   `A_B A_n` (with `A_n = sum_{j<n} 17^j 20^(n-1-j)`) is coprime to both.
   Clusters whose heads tie on `q` hold consecutive positions `mu_s..mu_e`
   and each receives `(sum c) / (mu_e - mu_s + 1)`; the total stays exactly
   one.
9. **Row.** 65,535 units by largest remainder over members in rank order, with
   equal remainders in roster order. The package carries every entrant's units, the mode,
   the promotion and each retained cluster's exact credit in head rank order;
   `check_outcome` recomputes them and refuses any difference, including a
   credit of equal value in other terms. At each tempo `derive_row` maps a
   member's hotkey and registration block to its current UID in the
   validator's snapshot, or else to the UID that the snapshot's finalized owner
   hotkey holds, which burns the units; the launch burn row sends all 65,535
   there. The row records that owner hotkey. An owner-hotkey rotation therefore
   moves the burn destination at the next tempo under the same target. If some
   units must burn and the finalized owner hotkey holds no UID, `derive_row`
   fails with `deferred`: that tempo's row waits for a later snapshot, nothing
   else pauses, and no other UID receives the units. A row with nothing to burn
   needs no owner UID.

The roster is bounded at 256 entrants (`MAX_ROSTER`).

## Copy policy: published artifacts and the near-copy relation

Every artifact is published when its cohort ends, and a copy of any published
artifact earns nothing.

- **Near-copy relation.** For two prediction vectors over the documents on
  which both returned a prediction (a document where either failed is left
  out), `aligned_rss(x, y)` is `RSS(x | y)`, the residual of `x` after the best
  non-decreasing function of `y`: documents grouped by equal `y` in ascending
  order, pool-adjacent-violators on `(sum, count)` blocks with a strict
  `S1 c2 > S2 c1` merge, and `RSS = sum x^2 - sum floor(S^2 / c)`, on the
  `10^-18` grid of the Brier utilities, all in `u128`. `near_stats` records
  `common`, `RSS(x | y)` and `RSS(y | x)`. Model `x` is a near copy of `y`
  (`near`) when `min(RSS(x|y), RSS(y|x)) <= common * d^2` with
  `d = 25,000,000` ppb, without a square root, and `x` does not score more than
  `w / 2` above `y`: `2 (S_x - S_y) 10^18 > w 10^36` is the exception
  (`outscores`). Any temperature, bias, clamp or other rank-preserving map is
  at distance zero. `NearStats::distance_ppb` is `floor(A_sym)`, an integer
  square root, used only for reporting and the monitor.
- **Exact copies.** `PublishedIndex` holds `PublishedContents` (every
  entrant's content from every closed cohort, plus every `R_0`) and the
  `FingerprintIndex`; each manifest binds both roots and counts
  (`PublishedIndexBinding`), and the package repeats the binding.
  `PublishedIndex::after_closure` builds the next cohort's index from a closed
  package and the closure's per-entrant `EntrantFingerprint`s: every entrant's
  content joins the set, and it requires exactly one fingerprint of each
  executed (scored) entrant on each probe version in use, gate-failed and
  low-scoring ones included, and none of an unexecuted one. Entrants with
  identical content can return different probe predictions (for example when
  one fails on a probe document); the canonical fingerprint of that content
  and probe version is then the earliest-committed entrant's, by finalized
  commitment block, then extrinsic index, then hotkey bytes
  (`canonical_fingerprints`), so every validator records the same one. An
  entrant whose content is in the set fails `public.content` at admission and
  is never executed (`score = None`); `finalize` refuses one that was.
- **Near copies.** Every scored entrant, whatever its score and other gates,
  carries a `PublicCheck`: its `NearStats` against `R_k` and (when `k >= 1`)
  `R_k-1` on the batch; its `ProbeMatch` comparisons with the fingerprints of
  `R_k` and then `R_k-1` on each probe version in use, in version order
  (`reference_probes`, ordered as `ScorePackageV2::reference_probe_keys`); and,
  only when neither reference is a near copy on the batch, the closest near
  `ProbeMatch` among every recorded fingerprint, both references' included
  (smallest distance, ties by content digest bytes, then version). The probe
  comparison therefore always includes `R_k` and `R_k-1`: a copy of a
  reference must escape both the batch and the 1,024-document probe, and at
  4,000 cases the probe is the binding check. There is no score floor:
  `R_k-1` can outscore `R_k` on a batch, and a copy meets new documents in a
  later cohort, so an entrant's score in its own cohort does not bound what a
  copy of it can earn later. A match fails `public.near.current`,
  `public.near.predecessor` or `public.near.probe`, in that precedence. Probe
  statistics name a probe version the manifest binds (`ProbeBinding`, at least
  one, at most 65,536 documents each) and carry both models' probe scores for
  the score clause, with `w` from the batch.
- **What validators check** (`check_public`, inside `evaluate` and
  `check_outcome`, given the coordinator's `PublishedIndex`, which must match
  the manifest's binding; validators do not yet rebuild it from every closed
  cohort's packages, so an index that left out a published content would go
  undetected): the index is
  canonical, every fingerprinted artifact is published, and it matches the
  package's binding and holds both references, each fingerprinted on every
  probe version in use; an entrant fails `public.content` exactly when its
  content is in the set (fully verified); statistics are present exactly for
  scored entrants, with predecessor statistics exactly when `k >= 1`, and
  every scored entrant carries exactly the reference probe comparisons above,
  in that order; batch statistics cover exactly the entrant's successful
  documents, probe statistics at most the probe's, and no residual exceeds
  `common * 10^18`; an entrant's own probe score agrees across its
  comparisons on a version, and each reference's probe score across entrants;
  each recorded near-copy failure, and each absence of one, follows from the
  published statistics and scores with the frozen `d` and `w / 2`, in the
  precedence above: an entrant near neither reference on the batch whose
  reference probe comparison is a near copy must fail `public.near.probe`
  with a match at least as close, and a match that names a reference repeats
  that comparison exactly; a probe match names a fingerprint the index holds.
  The statistics themselves, and the absence of a match among the other
  archived fingerprints, are coordinator measurements on hidden predictions,
  trusted like the scores.
- **Distance monitor**, all as `floor(A_sym)` in ppb:
  `min_gated_vs_current_ppb` is the smallest batch distance to `R_k` among
  gated entrants, which validators recompute from their statistics;
  `min_entrant_pair_ppb` is the smallest distance between two cluster heads, a
  coordinator measurement that must be absent with fewer than two clusters and
  never below `d`; `min_gated_vs_archive_ppb` is the smallest probe distance
  between a gated entrant and any fingerprinted published artifact, `R_k` and
  `R_k-1` included, on any probe version in use, a coordinator measurement
  that must be absent without a gated entrant, present whenever a gated
  entrant's published reference probe comparison has a common document, and
  never above the smallest of those comparisons. It shows the archive's
  closest pairs drifting toward `d` as it fills with same-recipe retrains.
- **Coordinator.** `finalize(package, index, &mut impl Measurements)` applies
  `public.content`, measures each scored entrant (`current`, `predecessor`, and
  `reference_probe` for both references on every probe version in use, all
  published; then `archive_probe`, the other fingerprints with the smallest
  distance over all of them, only when neither reference matched on the
  batch), records the gate, then the cutoff, the cluster walk (`pair`), the
  monitor and the outcome. It refuses a `reference_probe` result that names
  another fingerprint, and ignores archive comparisons that name a reference.
  Every scored entrant runs on each probe version in use for its own
  fingerprint, whether or not its comparison was needed. Probe storage,
  versioning, the probe-run phase and fingerprint maintenance are coordinator
  work outside this crate.
- **Blends are new models.** Output averages, weight soups and other mixtures
  of published artifacts are treated as new models and earn normally; the
  crate does not look for them. On the consumed Test a 50/50 blend of two
  published launch-recipe seeds scored 0.2 to 0.3 of a band above `R_0` at
  1,000 cases and cleared the gate against both sources, on the batch and on
  the probe, in every batch, so it earns in fallback cohorts like any model
  at that score.
- **Price of a perturbed copy** (consumed Test, statistics only). The cheapest
  copy of `R_0` found, rank-space noise after the best recalibration, clears
  the gate in more than 90% of 1,000-case batches for about 4e-4 to 5.5e-4 of
  score, 0.08 to 0.11 of a band. At 4,000 cases the batch alone is cleared for
  about 3e-4 to 4e-4, but the probe comparison with `R_k`'s fingerprint then
  binds, and clearing both in more than 99% of cohorts costs about 4.5e-4 to
  5.5e-4, about a fifth of a band. Such a copy can still earn in a fallback
  cohort; the paper states this limit.

## `testnet-reward-v0`

This is testnet shortcut T8. It stays testnet-only; Finney uses `reward-v1`.
It rewards confidence Brier only.

0. The rule is valid only on the testnet genesis `TESTNET_GENESIS`
   (`0x8f9cf856...263105`). Manifests, packages, targets and rows that name it
   on any other chain fail to encode, decode or derive (`mismatch`), so the
   shortcut fails closed on Finney.

1. Utility is `10^18 - (a - 10^9 y)^2`, with `a` the involvement probability
   in parts per billion, converted from model output by one multiplication
   rounded half to even. Non-finite or out-of-range output is invalid and never
   clamped.
2. The first attempt that is not an infrastructure error is final. A model
   error, invalid output or parse failure scores 0 and keeps its weight. If
   every attempt is an infrastructure error, the task is unresolved and the
   package cannot be built.
3. `S = floor(sum_g floor(sum_t u / n_g) / G)`: groups weigh equally and tasks
   within a group weigh equally.
4. `q = floor((S - S0) 10^18 / (10^18 - S0))` if `S > S0` and `S0 < 10^18`,
   otherwise 0.
5. Only entries whose exact `(uid, hotkey, registration_block)` is on the
   allow-list earn credit. Non-listed entries earn nothing, and their share is
   not redistributed.
6. Credit goes to the entry's UID if the validator's finalized snapshot holds
   the same identity there. After deregistration, UID reuse or a hotkey change
   it goes to UID 0. Allow-lists never contain UID 0.
7. If no credit is positive, including when the baseline is perfect, the row is
   `[(0, 65535)]`. UID 0 is the owner burn path. The manifest and package bind
   its exact `burn_identity` (`uid` 0, owner hotkey, registration block), and no
   row is derived unless the validator's snapshot holds that identity at UID 0.
8. Each weight is `floor((2 c 65535 + m) / (2 m))`, with `m` the largest credit
   (`c 65535 / m` rounded half up). The maximum is exactly 65535, so the chain's
   max-upscale leaves the row unchanged. Each entry is rounded on its own, so
   equal credits get equal weights and no tie order is needed. Zero weights are
   dropped, and entries are sorted by UID.
9. Arithmetic uses checked `u128` and fails closed with `overflow`.

## Active-target bundles

`target_bundle::verify` checks everything a validator, the coordinator
included, needs before it derives and commits the active target's row:

- the launch burn row under a `LaunchAuthorization`, or
- a cohort target with its manifest, score package, calculation target and
  signed closure, each authenticated by `Signed::open` before it is decoded;
  the published index as of the manifest (contents and fingerprint index),
  which must match the manifest's binding; the signed receipt index, bound to
  the package's root and count; and the reward-intent authorization, with an
  optional hint refresh, checked against its carrying cohort (the target's own, or
  a later cohort that produced no target and carried it).

`verify` recomputes the outcome with `check_outcome`, so every credit, every
member's units and the promotion walk are rebuilt and any difference is
refused before a row exists. A carrying cohort is verified the same way: its
outcome is recomputed against its own published index and must be
`NoTarget`, its receipt index is bound to its root and count, and its index
must still hold everything the target's cohort published and closed with.

The active target is the holder's, never the bundle's: `verify` takes the
target the holder holds active from its own durable record
(`ActiveTarget::restore`, the launch burn row before any activation) and
accepts only the held target (with the same activation epoch) or its
successor, activated by the bundle's own checked closure; the launch bundle
only while no cohort target has activated. The holder records
`VerifiedTarget::active` afterwards, so a replaced target or the launch
authorization after the first activation is refused from then on. A holder
that has not yet seen a newer bundle keeps committing the target it holds,
with no end, until a newer bundle supersedes it or a revocation withdraws it.
`VerifiedTarget::operation(keys, revocations, hotkey, epoch)` checks the
authorization again at every tempo against the holder's journaled
revocation records (each checked when journaled; one of another chain or
target withdraws nothing and is never an error) and returns the
`WeightOperationV2`; `VerifiedTarget::revoked_by`
names the revocation that withdraws it, `checked_terms` its digest, issue and
hint; and
`derive_row` derives the canonical row against the holder's own finalized
snapshot. A holder that produced the receipts (the coordinator) may leave the
receipt index out; every other validator includes it, and
`verify_receipt_chunks` checks the retained receipt chunk objects
(`u32_le(len) || receipt`, repeated): each receipt matches its index entry and
every entry is stored exactly once.

## Fixtures and cross-target check

`fixtures/golden-v1.txt` records the bytes, digests, signing payloads,
signatures, operation IDs, reward rows and rejection codes for a synthetic
`testnet-reward-v0` cohort on the testnet genesis.
`fixtures/golden-reward-v1.txt` records the same for `reward-v2` (the file
keeps its name), and `fixtures/golden-quant-v1.txt` the quantization's
vectors: stream blocks, `uniform` at edge moduli, `Bern_exp` sequences,
discrete-Laplace draws at the testnet scale, offsets, `q`, SE, the ladder and
budget terms, and q-only outcomes with a tie group, withheld rows and
roster-order leftovers. `reward-v2`'s report covers:

- The aligned distance on fixed synthetic vectors: identical, a
  rank-preserving recalibration, ties, a reversed order, failures left out, no
  common document, and the `w / 2` score clause at its boundary.
- A computed cohort: hidden predictions, each model with its own ranking of
  documents, become scores. An identical-content copy and a near copy join
  their original's cluster, and a tied independent cluster is dropped as
  near-equal. The published index holds `R_0`, `R_1` and an older artifact,
  each fingerprinted on a 20-document probe. Under the public-copy gate an
  exact copy of `R_k` is refused unexecuted, a recalibrated copy of `R_k` 0.035
  from it in raw RMS fails `public.near.current`, a sharpened copy of `R_k-1`
  fails `public.near.predecessor`, a copy of the older artifact fails
  `public.near.probe`, and an independent retrain at `R_k`'s score passes;
  entrants scoring below `S_Rk-1` are tested too. The lines record every
  statistic, including each entrant's comparisons with both reference
  fingerprints on the probe, the pairs that keep heads apart, the distance
  monitor and the next cohort's index, which fingerprints every executed
  entrant and, when identical contents return divergent probe predictions,
  records the earliest-committed one's.
- Two copy scenarios: a repackaged `R_k-1` that clears the margin cutoff
  because `R_k-1` outscores `R_k` on the batch fails
  `public.near.predecessor` and is neither promoted nor paid; and a later
  copy of an entrant that scored below `S_Rk-1` in its own cohort, which
  scores above it on new documents, fails `public.near.probe` against that
  entrant's fingerprint (without the fingerprint it would take the whole
  fallback row); and a perturbed copy of `R_k` that escapes the batch test is
  caught on the probe by `R_k`'s own fingerprint, although the coordinator's
  archive comparison left the references out. It also covers gate failures, an
  unexecuted entrant and attributable failures, plus a moved UID, a departed
  hotkey and a new registration generation, whose units burn through the
  owner's UID.
- Burn routing after an owner-hotkey rotation: the same targets send burned
  units to the new owner hotkey's UID, and an unregistered owner hotkey defers
  only rows that must burn.
- The package's exact cluster credits, and the closure's phase epochs against
  the admission, transition and cohort minimums, which a closure must pass
  before it activates a target.
- Reward intent without expiry: activation, commitments through and long
  past the renew-by hint with no renewal, a hint refresh that changes nothing,
  nothing before the authorization's issue, a signed revocation that
  withdraws the target from its epoch and a later authorization that
  re-authorizes it, a no-target cohort that authorizes the target it leaves
  live, the launch burn row carried by a no-target package,
  and the manifest-free launch authorization from the first tempo of cohort 1,
  after the first package while its target is pending, and refused once that
  target activates, both at its check and at the operation of one kept across
  the boundary. Operations re-check the active target: a validator with a
  stale one commits nothing, and a cohort authorization stops once a
  successor target activates.
- Submission commitments, payloads and envelopes, admission returning each
  roster entry and refusing another cohort, chain, hotkey or registration
  generation, and every entrant checked against its finalized commitment: the
  payload, the committing registration and UID, the block and extrinsic index,
  and the published object.
- Artifact objects: derived content, a copy with identical content, streaming
  in one-byte chunks, and malformed objects.
- The paper's worked examples: a lone cluster; 54.1/45.9 in one band or two;
  29.2/24.8/45.9; an honest cluster cut to 22.1% by six perturbed copies; the
  0.9415 fallback cutoff, in which an exact copy of `R_k` earns nothing; and
  the near-equal drop.
- Edge cases: all in one band, sybil clusters, score and remainder ties, every
  no-target case, extreme parameters, and Finney.
- The origin mix as a parameter (`mix.launch`), and the promotion audits: a
  clean audit, one flag and a clean re-audit, two flags passing promotion to
  the next head, a strict-tier failure doing the same, no head qualifying (no
  promotion, credits unchanged), and a canary clearance of a blocked head.
- Rejection codes, including a misapplied public-copy gate, distance `d`,
  `w / 2` clause, statistics coverage, precedence, probe match, a missing,
  reordered or ignored reference probe comparison, a probe match farther than
  a near reference, inconsistent probe scores, published index, an
  unfingerprinted reference or closure fingerprints that leave out an
  executed entrant or list one twice, cluster statistics and each distance
  monitor field, and the launch, reward-intent, hint-refresh and revocation
  refusals (hint distance, chain, target, active target, signature, an epoch
  before the authorization's issue, and a revocation dated before it).

The signing seeds are public test vectors, and the netuid is a placeholder.
`cargo test -p slop-ninja-protocol` compares both native reports with their
files and asserts each worked example's numbers.
`slop-ninja-protocol-wasm-fixture` builds both reports inside a Wasm module
that has no imports. `slop_ninja_protocol_check` runs that module in the
pure-Rust `wasmi` interpreter and requires each report to match the native
report and its golden file byte for byte:

```sh
cargo test --manifest-path subnet/Cargo.toml --locked -p slop-ninja-protocol
cargo build --manifest-path subnet/Cargo.toml --locked --release \
  --target wasm32-unknown-unknown -p slop-ninja-protocol-wasm-fixture
cargo run --manifest-path subnet/Cargo.toml --locked -p slop-ninja-protocol-check -- \
  wasm "$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/slop_ninja_protocol_wasm_fixture.wasm"
```

To regenerate a golden file after an intended, versioned change, run
`slop_ninja_protocol_check native > subnet/crates/protocol/fixtures/golden-v1.txt`
`slop_ninja_protocol_check native reward-v1 > subnet/crates/protocol/fixtures/golden-reward-v1.txt`
or `slop_ninja_protocol_check native quant-v1 > subnet/crates/protocol/fixtures/golden-quant-v1.txt`.

`tests/allocation.rs` bounds the largest allocation made while decoding an
oversized item-count claim, and checks that `Signed::open` rejects a forged
envelope before decoding it. `tests/whitepaper_numbers.rs` recomputes the
paper's statistical and cadence figures: promotion rates of 13%, 87% and 21%
for ten entrants equal to `R_k`, the 125,000-document noise floor, the
perturbed-copy costs against the near-copy distance (the figures of the
copy-policy wording proposed for the paper), the daily cadence, the
reward-intent and launch leases and payout delay, and the human stock. It is native-only and uses floating point freely, since it checks
the paper's arithmetic rather than protocol output. The crate itself uses
floating point only to convert input: `Probability::from_f64` turns a model
output into parts per billion with one correctly rounded multiplication,
rounded half to even, so every target agrees; the `testnet-reward-v0` fixtures
exercise it. Every score, credit and row is integer or rational arithmetic.

Validated on `build-host` (Linux x86_64, Rust 1.99.1) on 2026-10-29, and again
on 2026-10-30 after the tempo identity was added: all 14 unit tests and 3
allocation tests passed, and the release `wasm32-unknown-unknown` module
reproduced the 123-line golden report (SHA-256 `15b03504...64b4`) byte for
byte, as did the native report. Clippy passed with warnings denied on both
targets. macOS and the Workers runtime are not yet
exercised.

The `reward-v1` branch was validated on `build-host` (Linux x86_64, Rust 1.99.1)
on 2026-10-30. All 62 unit tests, 3 allocation tests, 6 whitepaper-number tests
and 3 doctests (two of them `compile_fail`) passed. The release and debug
`wasm32-unknown-unknown` modules each reproduced the unchanged 122-line `v1`
report (SHA-256 `a4fa3ea5...2d23`) and the 438-line `reward-v1` report (SHA-256
`51efe305...4b37`). Clippy passed with warnings denied for the protocol,
protocol-check and Wasm fixture crates, natively across all targets and for
`wasm32-unknown-unknown`, and the `no_runtime_gates` lint passed. The validator
(40 tests) and the native adapter (38 tests and the 2 `no_runtime_gates` tests)
passed against this crate with `--test-threads=1`, and the coordinator checked
for `wasm32-unknown-unknown` with its locked lockfile. macOS and the Workers
runtime are not yet exercised.

On main after both branches were merged (2026-10-31): 63 unit tests, 3
allocation tests, 6 whitepaper-number tests and 3 doctests passed. The release
`wasm32-unknown-unknown` module reproduced the 123-line `v1` report (SHA-256
`15b03504...64b4`, with the tempo identity) and the 438-line `reward-v1` report
(SHA-256 `51efe305...4b37`) byte for byte, as did the native reports and both
golden files. Clippy passed with warnings denied for the protocol and Wasm
fixture crates on `wasm32-unknown-unknown`.

On branch `subnet-testnet/reward-v1-live` (2026-10-31, after the origin mix,
the false-positive gate, the promotion audits, the clearance record and the
bundle verification were added, manifest and package version 4): 75 unit tests
(1 ignored: the report printer), 3 allocation tests, 6 whitepaper-number tests
and the doctests passed. The `reward-v1` golden report grew to 471 lines
(SHA-256 `75fc423e...ca32`); the native report and the release
`wasm32-unknown-unknown` module reproduced it byte for byte, and the
123-line `v1` report is unchanged (`15b03504...64b4`). Clippy passed with
warnings denied for the workspace natively and for the coordinator on
`wasm32-unknown-unknown`, and the `no_runtime_gates` lint passed.

## Not covered

- Chain call encoding and timelock payloads, including the commitment call
  that carries the submission payload.
- Release records, the coordinator's per-phase work authorization and key
  rotation procedure. Which reward-intent authorization is the latest, which
  revocations it holds and which published index applies to a cohort are the
  holder's state; the crate checks one authorization, its hint refresh and
  revocations, the target's
  activation against the held target, and one index against one package.
  Rebuilding each index from the previous one (`PublishedIndex::after_closure`)
  is available but not yet run by the validator or the coordinator's signing
  path, which trust the index the manifest binds.
- A no-target package's authorization of the launch burn row: `Authorizing::Cohort`
  needs a cohort target. A long launch regime needs nothing more: the launch
  authorization has no end.
- The coordinator's probe store and versioning, the probe-run phase,
  fingerprint maintenance and the archive comparison, which run in the scoring
  Container; the crate defines their records and checks their results.
- Validating an artifact's files (ONNX operators, tokenizer vocabulary,
  calibration) is qualification, outside this crate.
- Signed corrections of a held or invalid package.
- The lookup canary statistic and its anchors, and the canary and strict-tier
  measurements themselves, which the coordinator makes on hidden predictions;
  the crate checks their structure and the promotion they imply.
- Storage versions and migrations.
- Webhook ingress.

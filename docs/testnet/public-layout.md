# Public object layout (testnet)

Layout `v1` of the public status host for testnet subnet 601: the objects the
publisher writes, what each one holds, and how a validator or miner client
verifies them. The publisher is the cron Worker in
[`subnet/publisher`](../../subnet/publisher/README.md); this page is the
contract it implements. The validator fetcher reads only the authorization
pointer and the signed objects it names
([What a validator checks](#what-a-validator-checks)). The miner client's
`status` and `next-cohort` (with `--public`; without it they read the
coordinator's unsigned cohort read, `GET /admission/v1/cohort`) read these
paths.

**State: not yet live.** The publisher is built and tested, but not
deployed. The public host, its domain (decision D5) and public access are set
up at announce time. Until then, `R_0`'s reference package and its training
companion are attached to the testnet release as two tar archives that
extract to `R_0`'s package directory below
([`R_0` archives](miner-guide.md#r_0-archives-optional)); the S2c span
package is not downloadable until public access opens. `{public}` below is
the host's base URL.

## Rules

- Every object is static. The host serves the bucket's objects directly, and
  no Worker code runs per request.
- Paths never change once published. Artifacts, authorization objects and
  the signed cohort objects a publisher built before the copy model copies
  are immutable (`cache-control: public, max-age=31536000, immutable`).
  Index documents, pointers and the status snapshot are rewritten in place,
  and they and the cohort summaries are served with `cache-control: public,
  max-age=60`. A publisher built from this tree publishes no detailed
  receipt (no receipt index, chunk map or bundle), so nothing on the host is
  deleted under N+2. A publisher built before the copy model that publishes
  receipts (decision D7) also publishes the detailed receipts and deletes
  them after N+2 (see
  [Retention](#retention)), so it serves them with `cache-control: public,
  max-age=<one tempo in seconds>` and never as immutable: a deleted bundle
  leaves caches within a tempo.
- Every publisher verifies a cohort's objects in the private bucket only
  after the cohort closed, before it publishes anything for it
  ([What the publisher checks](#what-the-publisher-checks)). A publisher
  built from this tree then writes only the cohort's allow-listed summary
  and reads it back through the allow-list check; it copies none of the
  coordinator's signed records, indexes, components or receipts. A
  publisher built before the copy model copies the verified objects and
  verifies the copies again. Only then is the cohort listed in the index.
  **A cohort is published exactly when the cohort index lists it.** An
  object can be stored under a cohort directory before the cohort is
  listed, if a run stopped between that write and the index write; ignore
  any cohort the index does not list.
- Nothing published contains document text, labels, per-document
  predictions, spend figures or credentials. The
  [redaction rules](../../subnet/publisher/src/redact.rs) say how this is
  enforced. Layout v1 as described below also publishes values derived from
  the hidden evaluation: exact aggregate scores, near-copy and probe
  statistics, gate reasons and attempt counts in the signed package and the
  leaderboard, per-attempt outcome classes in the receipts, and strata in the
  manifest. A publisher built from this tree publishes only what the
  versioned allow-list names
  ([`allowlist.rs`](../../subnet/publisher/src/allowlist.rs)): per cohort an
  allow-listed summary of bands, gate bits and cluster membership, with the
  entrants in roster order, and the coordinator's signed row beside its
  authorization; no receipt, no score value and no rank field. The row still
  carries the ranking of retained entrants: until the quantization
  specification or a reward-rule change addresses it, its weights (and the
  weights validators reveal on chain) order the retained clusters within a
  band, whose credits decay by position, and give a cluster's leftover row
  units to its members in rank order. An entrant's artifact can steer the
  values layout v1 publishes, so they can reveal information about the
  hidden documents and their labels.
- Signed records are the coordinator's exact bytes. Verify them against the
  trust roots in the release's `testnet-trust.json`. The copy at
  `v1/trust/testnet-trust.json` is the same file byte for byte; never trust
  the host's copy alone.

## Paths

| Path | Content | Changes |
| --- | --- | --- |
| `index.html`, `style.css` | The leaderboard page, rendered from `v1/leaderboard.json` | when a cohort closes or an artifact lands |
| `v1/status.json` | The status snapshot (unsigned), schema `slop_ninja.public.status/v1` | every run (one a minute) |
| `v1/cohorts/index.json` | The cohort index, schema `slop_ninja.public.cohort_index/v1` | when a cohort is published or an artifact lands |
| `v1/cohorts/<label>/...` | One closed cohort's signed and content-free objects (below) | never, once listed |
| `v1/cohorts/<label>/publication.json` | The publisher's record of the cohort, schema `slop_ninja.public.publication/v1` | when an artifact lands |
| `v1/leaderboard.json` | The newest cohort's leaderboard, schema `slop_ninja.public.leaderboard/v1` | when a cohort closes or an artifact lands |
| `v1/reference/current.json`, `v1/reference/previous.json` | `R_k` and `R_k-1` after the newest cohort's close, schema `slop_ninja.public.reference/v1` | when a cohort closes |
| `v1/reference/packages/<sha256>/...` | One staged [reference package](reference-packages.md), named by its parts manifest's SHA-256: `artifact-files.json`, `parts/<sha256>`, `package.json`, `NOTICE.md`, `upstream/` and `reference/`, and for `R_0` its training companion `training/` | never, once staged |
| `v1/artifacts/<sha256>.bin` | An admitted outside entrant's artifact object, named by its SHA-256 | never, once written |
| `v1/authorizations/current.json` | The authorization validators commit under now, any renew-by hint refresh, the revocation set and the coordinator's signed row, schema `slop_ninja.public.authorization/v2` | when the coordinator installs an authorization, journals a revocation or signs a row |
| `v1/authorizations/<sha256>.signed` | One signed authorization, refresh, revocation set or coordinator row, named by its SHA-256 | never, once written |
| `v1/trust/testnet-trust.json` | The trust roots, byte-identical to the release's file | when the roots rotate (decision D20) |

### One cohort: `v1/cohorts/<label>/`

The object names are the coordinator's own. The same names are used in the
private receipts bucket under `cohorts/<label>/`.

These are coordinator records, and no validator built from this tree reads
them: a validator commits the coordinator's signed row
([What a validator checks](#what-a-validator-checks)). The table describes
what a publisher built before the copy model publishes. A publisher built
from this tree keeps them private and publishes only what its allow-list names (see
[Rules](#rules)).

| Object | Record | Verify with |
| --- | --- | --- |
| `manifest.signed` | `CohortManifestV2`, package key | `Signed::<CohortManifestV2>::open` |
| `package.signed` | `ScorePackageV2`, package key | `open`, then `check_manifest` |
| `target.signed` | `CalculationTargetV2`, package key; present exactly when the cohort produced a target | `open`, then `check_package` |
| `receipt-index.signed` | `ReceiptIndex`, receipt key | `open`, then `ScorePackageV2::check_receipts` |
| `chunk-map.json` | Format `slop_ninja_receipt_chunk_v1`: the receipt root and count, and each bundle's `key`, `sha256`, `bytes` and `receipts`; every key appears once and every bundle holds at least one receipt | digests and lengths; the root equals the signed index's digest |
| `receipts/bundle-job-<j>.bin` | Each job's receipts, `u32_le(len) \|\| AttemptReceipt` repeated | `ReceiptIndex::verify_object` for each receipt; each index entry exactly once |
| `published-contents.bin`, `fingerprint-index.bin` | The published index at the seal | its `binding()` equals the manifest's `published` |
| `closure.signed` | `CohortClosure`, package key: phase epochs, the target and every entrant's commitment opening | `open`, then `CohortClosure::check` |
| `closure-fingerprints.json` | `{"fingerprints": [{"hotkey", "version", "fingerprint"}]}`: each scored entrant's probe fingerprint digest | rebuilds the index after the closure |
| `published-contents-after.bin`, `fingerprint-index-after.bin` | The published index after the closure | equals `PublishedIndex::after_closure` of the index at the seal |
| `components.json` | Optional content-free diagnostics, schema `slop_ninja_eval_score_components_v2`, including the gate policy | published only when every field is one the schema names (unknown fields refused at every level), it agrees with the manifest and package field for field, its policy document hashes to the manifest's `gate_policy` digest, and it passes the redaction scan |

The chunk map lists bundle keys as private keys (`cohorts/<label>/receipts/...`),
byte for byte as the coordinator wrote them. To fetch a bundle, strip
`cohorts/<label>/` from its key and append the rest to
`{public}/v1/cohorts/<label>/`.

With decision D7 (b), receipt indexes, chunk maps and bundles are not
published (`PUBLISH_RECEIPTS = "false"`). A publisher built from this tree
refuses `PUBLISH_RECEIPTS = "true"` and never publishes them, and a
validator built from this tree reads none of them.

`publication.json` lists every object copied, with its path relative to the
cohort directory, its SHA-256 and its length. Before writing it, the
publisher verified each object, and then verified each copy. The record also
states the artifact state of every entrant (below), and why
`components.json` was withheld, when it was.

### What the publisher checks

Every publisher runs these checks on a closed cohort's objects in the
private bucket before it publishes anything for the cohort or lists it. Check
4 runs only where receipts are published (decision D7, a publisher built
before the copy model); a publisher built from this tree reads no receipt
and runs checks 1-3, 5 and 6. A publisher built before the copy model runs
them again on its copies. A validator never runs these checks
([What a validator checks](#what-a-validator-checks)). The coordinator's
`examples/reward_v1_verify.rs` runs the same checks.

1. The manifest is this chain's (testnet genesis, netuid 601), and its label
   is the directory's.
2. The package is bound to the manifest. The target, when present, is bound
   to the package.
3. The closure checks against the manifest's frozen schedule, the package and
   the target.
4. The receipt index is bound to the package's root and count. Every bundle
   in the chunk map has its listed digest and length. Every receipt cut from
   a bundle verifies against its index entry, and each entry is seen exactly
   once.
5. The published index at the seal matches the manifest's binding. The
   outcome recomputed with `reward_v1::check_outcome` equals the package's
   own (mode, promotion, units and credits).
6. The index after the closure equals its rebuild from the closure's
   fingerprints.

### Artifacts: `v1/artifacts/<sha256>.bin`

Artifacts that outside miners uploaded through the admission route are
published after their cohort closes, under the CC BY-SA 4.0 submission terms.
Each file is named by the SHA-256 that the score package binds
(`EntrantScore::artifact`). The destination bucket checks that digest on
write. Check it again after you download the file.

Publication is default-deny. The publisher copies an artifact only when the
coordinator's private eval bucket holds that entrant's upload envelope (the
protocol's `SubmissionUpload`) and the envelope names this chain and cohort,
the entrant's hotkey and registration, its committed content, commitment and
object, and the nonce the signed closure published. Before that check, it
withholds for good an artifact that:

- belongs to an operator or synthetic hotkey;
- is a listed reference digest;
- is an initial reference (`R_0` or its single replacement) that any
  manifest named, even after the manifest chain stopped naming it (the index
  lists them in `initial_references`). References are published as reference
  packages instead, never as entrant artifacts;
- failed the public-copy gate;
- is in a near-copy cluster with an artifact withheld for one of the
  reasons above. The whole cluster is withheld, whichever member is its
  head, so the reasons never single out the best-scoring member. Retries
  apply this to pending members too. A member withheld only after its
  siblings were published does not withdraw them: a published object is
  never recalled.

Each entrant's artifact has one of three states in `publication.json`, in
`leaderboard.json` and as counts in the index entry:

| State | Meaning |
| --- | --- |
| `published` | The object is at its path and matches the package's SHA-256. |
| `pending` | Not copied yet, with a reason. The object is not in the eval bucket yet, the admission object layout is not configured yet, the store did not answer, the run reached its wall budget, or (on a retry) the cohort summary that names its near-copy clusters is not readable. Later runs retry and repeat the checks above. |
| `withheld` | Never published, with the reason from the list above, or: no outside admission record, or a record that does not match the entrant. |

An artifact entry also carries the entrant's 64-hex hotkey, registration
block, commitment and opening nonce, as the signed package and closure state
them.

`v1/reference/current.json` names `R_k` by content digest and artifact
SHA-256. It has a `path` when `R_k` is a promoted miner artifact published
here. When `R_k` is a staged reference package (`R_0` and its replacement are
published this way), it has `package`, the package's directory, and
`licence`, `CC-BY-SA-4.0`; the publisher links a package only when the public
bucket holds its parts manifest and that manifest's SHA-256 is the
reference's artifact digest. Otherwise it has a `withheld` reason
(`reference package not yet staged`).

### Reference packages: `v1/reference/packages/<sha256>/`

The operator stages each reference package once; the publisher never writes
under this prefix. The directory is named by the package digest, the SHA-256
of its parts manifest:

| Object | Content |
| --- | --- |
| `artifact-files.json` | the parts manifest (`slop_ninja_a_artifact_files_v1`), exact bytes |
| `parts/<sha256>` | each part it lists, exact bytes, at most 64 MiB |
| `package.json` | `slop_ninja.public.reference_package/v1`: role, package, content and object digests, spec and graph class, the file table, licence and notice paths, and how to reassemble |
| `NOTICE.md`, `upstream/ModernBERT-base-LICENSE` | the CC BY-SA 4.0 notice with attribution, and the base model's licence |
| `reference/...` | the synthetic reference cases the export manifest binds, and their predictions |

Objects carry `cache-control: public, max-age=31536000, immutable`.
[Reference packages](reference-packages.md) lists each package's digests and
role.

## Authorizations

`v1/authorizations/current.json` names the authorization validators commit
under now and, when there are any, a refresh of its renew-by hint and the
revocation set:

```json
{
  "schema": "slop_ninja.public.authorization/v2",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "kind": "cohort",
  "authorization": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> },
  "renewal": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> },
  "revocations": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> },
  "row": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> }
}
```

- `kind` is `launch` (a `LaunchAuthorization` of the launch burn row) or
  `cohort` (a `RewardIntentAuthorization`); `renewal` is a
  `RewardIntentRenewal` of that authorization, or absent; `revocations` is
  the revocation set, or absent (the key is left out): the latest signed
  `RewardIntentRevocation` of every revoked target, oldest first,
  concatenated. Every revocation has the same fixed signed length on a
  chain, so `bytes` is a whole multiple of it and the entries are cut at
  that length. A validator that missed earlier revocations (offline, or
  still holding an older target) journals every entry, so the set reaches it
  whatever target it holds. Schema v2 replaced v1's single latest
  `revocation`; a validator built for v1 refuses a v2 pointer by its schema,
  cannot see revocations, and must be upgraded.
- `row` is the coordinator's signed row (`CoordinatorRow`, authorization
  key) under the authorization the pointer names, or absent (the key is
  left out) until the coordinator signs one under it. A publisher built from
  this tree copies it only once it verifies; otherwise it publishes the
  pointer without `row`, and validators keep the row they hold.
- No authorization expires. Its `renew_by_epoch` is a non-binding hint: an
  authorization keeps validators committing its target until a newer one
  supersedes it or a revocation withdraws it (every authorization of the
  revocation's target issued at or before the revocation's epoch). Validators
  journal the revocation set before anything else, and skip an entry that
  does not verify under their trust roots.
- Each object is content-addressed and signed with the authorization key.
  Check its SHA-256 and length, then open it under the trust roots.
- A publisher built from this tree copies an authorization, its row and the
  revocation set as soon as they verify, before and independently of any
  cohort listing. A publisher built before the copy model copies a `cohort`
  authorization only once the cohort that carries it is listed in the index,
  and it does not know `row`: it refuses a private pointer that carries
  `row` (unknown fields are refused) and copies nothing from it, so the
  public pointer, revocations included, stays as it was. The copy-model
  publisher therefore deploys before the coordinator that signs rows (the
  copy-model deploy order in
  [`TESTNET.md`](../../subnet/coordinator/TESTNET.md)).
- The pointer is rewritten when the coordinator installs an authorization,
  journals a revocation or signs a row. It has no validity window. Objects are never
  deleted.

### What a validator checks

A validator built from this tree reads the pointer and the objects it
names, and nothing else: no cohort object, receipt or index. It checks:

1. each listed length against its record's bound before the object is
   requested, and each object against its listed SHA-256 and length before
   it is used;
2. the revocation set first: each entry under the pinned authorization key,
   journaled before anything else (an entry that does not verify is
   skipped);
3. the authorization, any renewal and the row under the pinned trust roots,
   each naming this chain;
4. that the row names the authorization's record digest, target, carrying
   cohort and manifest digest, and issue epoch;
5. that a launch row comes only while the held target is the launch burn
   row, and that a cohort row's target is the held one, with the same
   activation epoch, or one that activates later.

Every per-tempo commitment then checks the authorization again against the
journaled revocations.

The [validator guide](validator-guide.md#fetching-the-coordinators-row)
says what it does with a verified row.

### The publisher's source

The publisher's source is the coordinator's private pointer
`authorizations/current.json` in the receipts bucket, with the same schema
and private object keys `authorizations/<sha256>.signed`. The publisher
checks each object's digest, length and signature, that a renewal renews
the named authorization and that a revocation names this chain, before it
copies anything.

## The status snapshot: `v1/status.json`

The snapshot is unsigned and is information only. The coordinator's
`GET /admission/v1/cohort` is authoritative for uploads. The fields that the
miner client reads ([admission API](admission-api.md), section 10):

```json
{
  "schema": "slop_ninja.public.status/v1",
  "network": "testnet",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "confirmed": { "block": 8340012, "epoch": 23170, "age_secs": 40 },
  "phase": "admission",
  "mode": "burn",
  "fault": false,
  "notice": "Launch burn: … Testnet: …",
  "admitting": { "cohort": 7, "label": "testnet-c7", "opened_epoch": 23169, "planned_cutoff_epoch": 23170 },
  "next": { "cohort": 8, "earliest_open_epoch": 23189, "admission_tempos": 16 },
  "coordinator": "https://…",
  "active_target": "<64 hex>",
  "reference": "<64 hex>"
}
```

Further fields, which the client ignores:

| Field | Meaning |
| --- | --- |
| `reference_index` | `k` of the current reference `R_k` |
| `generated_at` | Unix seconds of the run that wrote the snapshot |
| `sources` | Whether the coordinator's and the eval Worker's status answered this run |
| `last_published_cohort` | The newest cohort in the index |

Field meanings:

| Field | Meaning |
| --- | --- |
| `phase` | `admission` while the active cohort's window is open or its roster is not yet frozen; `evaluating` from the roster freeze until the cohort closes; `queued` when only a queued cohort waits; `idle` when there is none; `unknown` when the status could not be read. The work phases (generation, sealing, scoring, packaging, transition) are not published by name. A reward cohort closes at a tempo boundary; a probe build closes when its work commits, within a tempo. The phase name is the only thing hidden: a cohort's records (and `last_published_cohort`) are published within a publisher run of its package becoming durable, and `fault` turns on when a step fails, so the snapshot as a whole still shows when some hidden work finished. |
| `mode` | `burn` while the launch burn row is the active target; `target` once a cohort target is active |
| `fault` | `true` when any durable fault is active, or the publisher could not read the coordinator's status. No fault detail is published. |
| `admitting.planned_cutoff_epoch` | The opening epoch plus the cohort's admission minimum. Tempos that pass while the coordinator is silent do not count, so the actual cutoff can be later. |
| `next.earliest_open_epoch` | A lower bound: the earliest epoch at which the cohort before it can close (at least `K` tempos after it opened and its transition minimum after its publication), or the next epoch when no cohort is open. The next cohort opens at that cohort's close. |
| `next.admission_tempos` | How many tempos the next cohort's admission window lasts once it opens (its queued plan's value, else the standing template's); absent when unknown |
| `notice` | The burn regime in plain words; the statement that testnet scores test plumbing and that evaluation documents may be reused and exposed (decision D8); any operator notice |

A snapshot older than a few minutes (`generated_at`) means the publisher has
stopped. The operator is alerted when that happens: the publisher posts a
heartbeat to a separate private alerts Worker after every run whose alert
phase finished, and that Worker's own cron alerts the operator, through the
operator's private alert feed, read from outside Cloudflare, when the
heartbeats stop (see `subnet/alerts-relay/README.md`). It never touches the
public host.

## The cohort index: `v1/cohorts/index.json`

```json
{
  "schema": "slop_ninja.public.cohort_index/v1",
  "network": "testnet",
  "chain": { "genesis": "…", "netuid": 601, "mechanism": 0 },
  "trust": { "path": "v1/trust/testnet-trust.json", "sha256": "…" },
  "active_target": { "digest": "…", "source": "cohort", "cohort": 7 },
  "cohorts": [
    {
      "cohort": 7, "label": "testnet-c7", "path": "v1/cohorts/testnet-c7/",
      "opened_epoch": 23169, "admission_closed_epoch": 23170, "published_epoch": 23185, "closed_epoch": 23189,
      "mode": "margin", "promoted": "5F…", "target": "…", "reference_index": 0,
      "manifest": "…", "package": "…", "closure": "…",
      "entrants": 12, "receipts": 30144, "receipts_retained": true,
      "artifacts_published": 11, "artifacts_withheld": 0, "artifacts_pending": 1
    }
  ]
}
```

Cohorts are listed in ascending order, and strictly in order: a cohort the
publisher cannot read yet, or a packaged cohort whose closure is not written
yet, holds every later one, so a later cohort (and its target and
authorization) is never listed before an earlier one. A packaged cohort that
was abandoned holds later ones until the coordinator's retirement marker for
it (due after two later completions) shows that no closure follows; a signed
abandonment record that releases it at once is planned but not written yet.
A closed
cohort the publisher refused is not listed but appears in `refused`, a
content-free list of `label`, `cohort` and `opened_epoch` (when its manifest
opened), `starts_record` (its reference is `R_0` with no predecessor),
`reason` (the object and the kind of failure, for example
`package.signed: does not verify`) and the publisher `build` that refused it,
with `receipts_deleted_after` once detailed receipts its copy left on the
host were deleted under the coordinator's marker; later cohorts are
listed after it, and a later publisher build that accepts it lists it in its
place by number and drops the entry. One exception: before the record has
started, a refused cohort that cannot start it (or whose manifest did not
open) starts nothing, so later cohorts wait until a cohort that starts the
record is listed, retired or refused, or a new publisher build accepts the
refused one. `retired_unpublished` lists closed
cohorts whose detailed receipts the coordinator deleted before the publisher
copied them. `manifest`, `package` and `closure`
are record digests, so a fetcher can check that it holds the records the index
names. `active_target` is the target of the last listed cohort that produced
one, or the launch burn row (`source: "launch_burn"`) if no listed cohort has
produced one yet. `initial_references` lists each initial reference (`R_0`
or its replacement) a listed cohort named, by content digest and artifact
SHA-256, with the first cohort that named it.

The public record (decision U2's restarted record) holds the closed cohorts
whose manifest opened at or after the epoch the operator configures
(`PUBLISH_FROM_EPOCH`), and, when a label prefix is configured, whose label
starts with it. Chain epochs only increase, so a restarted coordinator that
numbers its cohorts from 1 again cannot bring rehearsal cohorts into the
record. The record starts at a cohort whose reference is `R_0` with no
predecessor, and cohort numbers order like opening epochs. Rehearsal and load
cohorts are never listed. When the label prefix changes (a chain-record
restart), the earlier record's index is archived at
`v1/records/<its first label>/cohorts/index.json` (the index only: the live
leaderboard and reference pointers are removed, not archived), and a fresh
index starts that records the earlier record, its initial references (still
withheld as entrant artifacts) and its cohorts whose detailed receipts are
still published (deleted once the coordinator's retirement marker for each
exists; see [Retention](#retention)).
Components of
a cohort whose plan names an artifact spec carry `limits`, the limits record
(`slop_ninja_eval_limits_v2`) the signed manifest binds by digest under
`slop_ninja.eval/limits/v2`.

### Retention

A publisher built from this tree publishes no detailed receipt, so the
public host has no retention rule and that publisher deletes nothing under
N+2; the coordinator's N+2 rule applies to its private receipts. The rest of
this section describes a publisher built before the copy model that
publishes receipts (decision D7).

Detailed receipts follow the coordinator's N+2 rule on the public host. The
coordinator deletes cohort N's private detailed receipts once N+1 and N+2
complete and N's target is superseded, holding the active target's cohort
and the cohort whose target the current authorization commits, and first
writes a content-free marker for N. The publisher deletes the public copies
of N's `receipt-index.signed`, `chunk-map.json` and `receipts/` bundles only
once that marker exists, never on its own count, so the public and private
copies follow one decision. The public side can only delay that decision:
while the public index's `active_target`, or the target the public current
authorization commits, is still N's (the publisher has not yet listed the
superseding cohort or copied the newer authorization), N's public receipts
stay, and they go on the marker once it caught up. When it deletes them, it
sets `receipts_retained: false` in the
index entry and `receipts_deleted_after` (the marker's due-after cohort) in
the publication record. The manifest, package (with the receipt root and count), target,
closure, published indexes and artifacts stay (the artifacts of `R_k` and
`R_k-1` included). Caches keep a deleted bundle for one tempo at most (see
[Rules](#rules)). Nothing else is ever deleted. A validator built from this
tree reads no receipt.

## The leaderboard: `v1/leaderboard.json`

The newest listed cohort. Each value is taken from the verified package and
from the outcome the publisher recomputed. One row per entrant: gated
entrants first in rank order, then the others in roster order.

| Field | Meaning |
| --- | --- |
| `rank` | Position among gated entrants, from 1; `null` for a gate failure |
| `uid`, `hotkey`, `hotkey_hex` | The entrant's UID at the commitment, and its SS58 and hex hotkey |
| `score` | The mean binary Brier utility over the batch: `decimal` (six places, rounded down) and `raw` (exact, in units of 10^-18) |
| `assigned`, `succeeded`, `failed` | Documents assigned, answered, and attributable failures |
| `gates` | Gate failure reason codes; empty when every enforced gate passed |
| `copy` | The public-copy outcome (`independent`, `not_executed`, or a `public.*` reason), and the aligned distance to `R_k` on the batch in parts per billion |
| `cluster` | The near-copy cluster's head (SS58 and UID), how this entrant joined it (`head`, `identical`, `near`), the distance for `near`, and the member count |
| `band` | The absolute band `b`, `kappa` (`band_rank`) and `mu` (`rank_in_band`) of the entrant's cluster |
| `credit` | The cluster's exact credit `c`, this member's share `c / m` exactly, and that share as a decimal |
| `units`, `row_share` | Units of the 65,535 row, and the fraction of the row |
| `artifact` | The artifact's state and path, as above |

The document also carries:

- the cohort's mode;
- the cutoff and the band width;
- the reference's and predecessor's scores;
- the promoted hotkey;
- `promotion_rule`: whether promotions run the lookup canary
  (`lookup_canary`, `false` on testnet), its `scope` (`testnet-only`) and a
  `note`, which the page shows. Testnet promotions run without the lookup
  canary (decision D12b), though an operator drill named in the cohort's
  plan can still block a candidate; the canary is required before mainnet;
- the active target;
- the reference pointers after this close.

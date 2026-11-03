# Public object layout (testnet)

Layout `v1` of the public status host for testnet subnet 601: the objects the
publisher writes, what each one holds, and how a validator or miner client
verifies them. The publisher is the cron Worker in
[`subnet/publisher`](../../subnet/publisher/README.md); this page is the
contract it implements. W7's validator fetcher and the miner client's
`status` and `next-cohort` read these paths.

**State: not yet live.** The publisher is built and tested, but not
deployed. The public host, its domain (decision D5) and public access are set
up at announce time. `{public}` below is the host's base URL.

## Rules

- Every object is static. The host serves the bucket's objects directly, and
  no Worker code runs per request.
- Paths never change once published. Signed cohort objects, artifacts and
  authorization objects are immutable (`cache-control: public,
  max-age=31536000, immutable`). Detailed receipts (the receipt index, the
  chunk map and the bundles) are deleted after N+2 (see
  [Retention](#retention)), so they are served with `cache-control: public,
  max-age=<one tempo in seconds>` and never as immutable: a deleted bundle
  leaves caches within a tempo. Index documents, pointers and the status
  snapshot are rewritten in place (`cache-control: public, max-age=60`).
- The publisher copies a cohort's objects only after the cohort closed, and
  only after it has verified them as a validator would (below). It verifies
  the copies again, and only then lists the cohort in the index. **A cohort is
  published exactly when the cohort index lists it.** An object can be stored
  under a cohort directory before the cohort is listed, if a run stopped
  between the copy and the index write; ignore any cohort the index does not
  list.
- Nothing published contains document text, labels, per-document
  predictions, hidden evaluation data, spend figures or credentials. The
  [redaction rules](../../subnet/publisher/src/redact.rs) say how this is
  enforced.
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
| `v1/authorizations/current.json` | The authorization validators commit under now, and its renewal, schema `slop_ninja.public.authorization/v1` | when the coordinator issues or renews one |
| `v1/authorizations/<sha256>.signed` | One signed authorization or renewal, named by its SHA-256 | never, once written |
| `v1/trust/testnet-trust.json` | The trust roots, byte-identical to the release's file | when the roots rotate (decision D20) |

### One cohort: `v1/cohorts/<label>/`

The object names are the coordinator's own. The same names are used in the
private receipts bucket under `cohorts/<label>/`.

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
published (`PUBLISH_RECEIPTS = "false"`), and validators get them another
way.

`publication.json` lists every object copied, with its path relative to the
cohort directory, its SHA-256 and its length. Before writing it, the
publisher verified each object, and then verified each copy. The record also
states the artifact state of every entrant (below), and why
`components.json` was withheld, when it was.

### Verification order

This is what the publisher checks before it publishes, and what a validator
checks after it fetches. The coordinator's
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
- joined the near-copy cluster of a withheld artifact.

Each entrant's artifact has one of three states in `publication.json`, in
`leaderboard.json` and as counts in the index entry:

| State | Meaning |
| --- | --- |
| `published` | The object is at its path and matches the package's SHA-256. |
| `pending` | Not copied yet, with a reason. The object is not in the eval bucket yet, the admission object layout is not configured yet, the store did not answer, or the run reached its wall budget. Later runs retry and repeat the checks above. |
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
under now and, when there is one, its latest renewal:

```json
{
  "schema": "slop_ninja.public.authorization/v1",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "kind": "cohort",
  "authorization": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> },
  "renewal": { "path": "v1/authorizations/<sha256>.signed", "sha256": "<64 hex>", "bytes": <length> }
}
```

- `kind` is `launch` (a `LaunchAuthorization` of the launch burn row) or
  `cohort` (a `RewardIntentAuthorization`); `renewal` is a
  `RewardIntentRenewal` of that authorization, or absent.
- Each object is content-addressed and signed with the authorization key.
  Check its SHA-256 and length, then open it under the trust roots.
- A `cohort` authorization appears only once the cohort that carries it is
  listed in the index, with the manifest and package digests it names.
- The pointer is rewritten when the coordinator issues or renews an
  authorization. Objects are never deleted.

The publisher's source is the coordinator's private pointer
`authorizations/current.json` in the receipts bucket, with the same schema
and private object keys `authorizations/<sha256>.signed`. The publisher
checks each object's digest, length and signature, and that a renewal renews
the named authorization, before it copies anything.

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
  "next": { "cohort": 8, "earliest_open_epoch": 23189 },
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
| `phase` | The active cohort's phase (`admission`, `generating`, `sealing`, `scoring`, `packaging`, `transition`); `queued` when only a queued cohort waits; `idle` when there is none; `unknown` when the status could not be read |
| `mode` | `burn` while the launch burn row is the active target; `target` once a cohort target is active |
| `fault` | `true` when any durable fault is active, or the publisher could not read the coordinator's status. No fault detail is published. |
| `admitting.planned_cutoff_epoch` | The opening epoch plus the cohort's admission minimum. Tempos that pass while the coordinator is silent do not count, so the actual cutoff can be later. |
| `next.earliest_open_epoch` | The active cohort's earliest closing epoch, or the next epoch when no cohort is active |
| `notice` | The burn regime in plain words; the statement that testnet scores test plumbing and that evaluation documents may be reused and exposed (decision D8); any operator notice |

A snapshot older than a few minutes (`generated_at`) means the publisher has
stopped. The dead-man heartbeat pages the operator when that happens.

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

Cohorts are listed in ascending order. `manifest`, `package` and `closure`
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
cohorts are never listed.

### Retention

Detailed receipts follow the N+2 rule on the public host. (The deletion of
the coordinator's private copies is the coordinator's and is not yet
implemented.) Once two later cohorts are listed (N+1 and N+2 complete) and
N's target, if it produced one, is superseded by a later cohort's target, the
publisher deletes cohort N's `receipt-index.signed`, `chunk-map.json` and
`receipts/` bundles. While N's target is the active target, its receipts
stay. It then sets `receipts_retained: false` in the index
entry and `receipts_deleted_after` (the N+2 cohort) in the publication
record. The manifest, package (with the receipt root and count), target,
closure, published indexes and artifacts stay (the artifacts of `R_k` and
`R_k-1` included). New commitments need retained
receipts, so fetch them while the cohort is current. Caches keep a deleted
bundle for one tempo at most (see [Rules](#rules)). Nothing else is ever
deleted.

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
- the active target;
- the reference pointers after this close.

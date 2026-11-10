# Public object layout (testnet)

Layout `v1` of the public status host for testnet subnet 601: the objects the
publisher writes, what each one holds, and how a validator or miner client
verifies them. The publisher is the cron Worker in
[`subnet/publisher`](../../subnet/publisher/README.md); this page is the
contract it implements. The validator fetcher reads only the authorization
pointer and the signed objects it names
([What a validator checks](#what-a-validator-checks)). The miner client's
`status` and `next-cohort` (with `--public`; without it they read the
coordinator's unsigned cohort read, `GET /admission/v2/cohort`) read these
paths. The JSON status API the website serves from the dashboard documents
is described [at the end](#the-public-status-api-slopninjaapi).

**State: public access not yet enabled.** The publisher is deployed and
writes this layout to the testnet public bucket, but the bucket is not yet
readable: the public host, its domain (decision D5) and public access are set
up at announce time. Until then, `R_0`'s reference package and its training
companion are attached to the testnet release as two tar archives that
extract to `R_0`'s package directory below
([`R_0` archives](miner-guide.md#r_0-archives-optional)); the span
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
  enforced. A publisher built from this tree publishes only what the
  versioned allow-list (`slop_ninja.public.allowlist/v3`,
  [`allowlist.rs`](../../subnet/publisher/src/allowlist.rs)) names. Per
  cohort that is a summary ([below](#the-cohort-summary-summaryjson)) of
  the coordinator's quantized scores under `quant-v1`: the grid width,
  each entrant's `q`, band, gate bit, public-copy gate bit and cluster
  membership, with the entrants in roster order, and `R_k`'s and
  `R_k-1`'s `q`. The coordinator's signed row is published beside its
  authorization. Nothing else computed from the hidden predictions is
  published: no exact score, SE, quantization offset or noise, near-copy
  or probe statistic, distance monitor, attempt or failure count, receipt
  or rank field. Rewards are computed from the quantized scores alone
  (`reward-v2`), so the row and the weights validators reveal on chain
  follow from the published `q` values, bits and membership, and carry no
  order beyond them. Layout v1 as a publisher built before the copy model
  wrote it also published exact aggregate scores, statistics, gate reasons
  and attempt counts, per-attempt outcome classes and strata; an entrant's
  artifact can steer such values, so they can reveal information about the
  hidden documents and their labels.
- Signed records are the coordinator's exact bytes. Verify them against the
  trust roots in the release's `testnet-trust.json`. The copy at
  `v1/trust/testnet-trust.json` is the same file byte for byte; never trust
  the host's copy alone.

## Paths

| Path | Content | Changes |
| --- | --- | --- |
| `index.html`, `style.css` | The leaderboard page, rendered from `v1/leaderboard.json` (no score column) | with `v1/leaderboard.json` |
| `v1/status.json` | The status snapshot (unsigned), schema `slop_ninja.public.status/v1` | every run (one a minute) |
| `v1/cohorts/index.json` | The cohort index, schema `slop_ninja.public.cohort_index/v1` | when a cohort is published or an artifact lands |
| `v1/cohorts/<label>/summary.json` | One closed cohort's quantized summary, schema `slop_ninja.public.cohort_summary/v3` (below) | never, once listed |
| `v1/cohorts/<label>/artifacts.json` | Publication at scoring start: each roster entry's artifact state, header digest and published keys, schema `slop_ninja.public.artifacts/v1` | from scoring start until every entry is decided |
| `v1/cohorts/<label>/publication.json` | The publisher's record of the cohort, schema `slop_ninja.public.publication/v1` | when an artifact lands |
| `v1/leaderboard.json` | The newest listed cohort's leaderboard, schema `slop_ninja.public.leaderboard/v4`: its summary's rows in roster order with each artifact's state, no rank or exact score ([below](#the-leaderboard-v1leaderboardjson)) | when the newest cohort is listed or one of its pending artifacts is settled; deleted at a record restart |
| `v1/reference/current.json`, `v1/reference/previous.json` | `R_k` and `R_k-1` after the newest cohort's close, schema `slop_ninja.public.reference/v1` | with `v1/leaderboard.json` |
| `v1/reference/packages/<sha256>/...` | One staged [reference package](reference-packages.md), named by its parts manifest's SHA-256: `artifact-files.json`, `parts/<sha256>`, `package.json`, `NOTICE.md`, `upstream/` and `reference/`, and for `R_0` its training companion `training/` | never, once staged |
| `artifacts/v2/<sha256>/header` | An admitted entrant's artifact header (the file table), named by its SHA-256, the digest the cohort's result names | never, once written |
| `blobs/sha256/<sha256>` | One artifact file, named by its SHA-256 and shared by every artifact that lists it | never, once written |
| `v1/authorizations/current.json` | The authorization validators commit under now, any renew-by hint refresh, the revocation set and the coordinator's signed row, schema `slop_ninja.public.authorization/v2` | when the coordinator installs an authorization, journals a revocation or signs a row |
| `v1/authorizations/<sha256>.signed` | One signed authorization, refresh, revocation set or coordinator row, named by its SHA-256 | never, once written |
| `v1/trust/testnet-trust.json` | The trust roots, byte-identical to the release's file | when the roots rotate (decision D20) |

### One cohort: `v1/cohorts/<label>/`

A listed cohort's directory holds `summary.json` and `publication.json`,
and `artifacts.json` from scoring start (below), only. The coordinator keeps its other records private, under the same
names in the private receipts bucket at `cohorts/<label>/`:

| Object | Record | Read by the publisher |
| --- | --- | --- |
| `manifest.signed` | `CohortManifestV2`, package key | yes: the chain, label and roster |
| `result.signed` | `QuantizedResult`, package key: the cohort's public record under `quant-v1` (below) | yes |
| `closure.signed` | `CohortClosure` (version 2), package key: phase epochs, the target, the result's digest and every entrant's commitment opening | yes |
| `package.signed` | `ScorePackageV2`, package key: the exact scores, every hidden measurement, the SE sums and the quantization draws | never |
| `components.json` | the coordinator's private diagnostics | never |
| `target.signed`, `receipt-index.signed`, `chunk-map.json`, `receipts/...`, the published-content and fingerprint indexes, `closure-fingerprints.json` | targets, receipts and indexes | never |

A validator built from this tree reads none of them: it commits the
coordinator's signed row
([What a validator checks](#what-a-validator-checks)).

`QuantizedResult` (kind `quantized-result`, version 1) carries the chain,
cohort, label, manifest digest and opening epoch; the rules `reward-v2`
and `quant-v1`; the batch size `n`, the charged count `m`, the grid width
`w`, the band width `W_q` and the public reward and quantization
parameters; the cutoff mode and cutoff; each reference's content, artifact
and `q`; the promoted hotkey; the occupied bands' grid edges; per entrant
in roster order its UID, hotkey, registration, commitment, content and
artifact (all public before scoring), its outcome (`NotExecuted`,
`Withheld`, or `Scored {q, gate_pass, copy_pass, cluster_first_member}`),
band rank and units; and the bit budget's counted terms. Readers refuse a
declared length above `QuantizedResult::max_signed_len()` before reading
it. Opening it validates it: every `q` is a multiple of `w`, the charged
count and grid are the rule's, and the cutoff, bands, band ranks, units and
bit budget are recomputed from the published values alone
(`reward_v1::derive_units`).

`publication.json` names the summary by SHA-256 and states the artifact
state of every entrant (below).

### What the publisher checks

Every publisher runs these checks on a closed cohort's objects in the
private bucket before it publishes anything for the cohort or lists it. A
validator never runs them
([What a validator checks](#what-a-validator-checks)).

1. The manifest is this chain's (testnet genesis, netuid 601), and its label
   is the directory's.
2. The result opens under the package key, validates (above), and is the
   one the manifest frames: its digest, chain, cohort, label, opening epoch,
   rule, parameters, batch size, references and roster
   (`QuantizedResult::check_manifest`).
3. The closure names the result by digest, has a target exactly when the
   result's mode produced one, and opens every entrant's commitment
   (`CohortClosure::check_result`), and its phase epochs meet the
   manifest's frozen schedule (`CohortClosure::check_schedule`).

The coordinator checks the rest privately before it signs: the package
against the manifest, the target and receipts against the package, the
outcome recomputed from the package, every quantization draw replayed from
the private seed (`reward_v1::check_draws`), and the published index after
the closure.

### The cohort summary: `summary.json`

Schema `slop_ninja.public.cohort_summary/v3`, built from the verified
result alone:

```json
{
  "schema": "slop_ninja.public.cohort_summary/v3",
  "network": "testnet",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "cohort": 12, "label": "testnet-c12", "opened_epoch": 23100, "closed_epoch": 23120,
  "quantization": { "rule": "quant-v1", "width": "0.03" },
  "references": {
    "index": 1,
    "current": { "content": "<64 hex>", "artifact": "<64 hex>", "q": "0.84" },
    "predecessor": { "content": "<64 hex>", "artifact": "<64 hex>", "q": "0.81" }
  },
  "quality": {
    "cutoff": { "q": "0.87", "mode": "margin" },
    "bands": [ { "band": 1, "lower": "0.90", "upper": "0.96" } ]
  },
  "promoted": "<ss58>",
  "entrants": [
    { "uid": 7, "hotkey": "<ss58>", "hotkey_hex": "<64 hex>", "q": "0.90", "band": 1,
      "gate": "pass", "copy_gate": "pass", "cluster": { "uid": 7, "hotkey": "<ss58>" } }
  ]
}
```

- Every score is a multiple of `width`, printed with exactly its decimals
  (`width = digit * 10^-d` gives `d` decimals); a band's `upper` may be
  `1` printed so.
- `gate` is `pass` (every gate passed, the public-copy gate included),
  `fail`, `withheld` (the minimum-success gate: `q`, `band`, `copy_gate` and
  `cluster` are `null`) or `not_executed`.
- `band` is `1` for the top occupied band; it is set for an entrant at or
  above the cutoff. `cluster` names the cluster's first member in roster
  order.
- `quality.cutoff` is `null` and `bands` empty for a cohort without a
  target; `mode` is `margin` or `fallback`. Band edges are absolute grid
  multiples of `W_q`.
- No field carries a rank. The q-rank (`q` descending, then roster order)
  follows from the published values.

The cohort index entry of a listed cohort also carries `best_q` (the
highest `q` at or above the cutoff) and the cohort's `quantization` block.
`v1/leaderboard.json` (schema `slop_ninja.public.leaderboard/v4`,
[below](#the-leaderboard-v1leaderboardjson)) carries the newest cohort's
`quantization` block, its references with their `q`, and its entrants as
the summary lists them, without the `quality` block.

### Artifacts: `artifacts/v2/<sha256>/header` and `blobs/sha256/<sha256>`

Every admitted entrant's artifact is published when its cohort's scoring
starts (the roster is sealed and the coordinator's signed manifest written),
under the CC BY-SA 4.0 submission terms. An artifact is its header (the file
table: the output contract and each file's name, role, SHA-256 and length)
and its files:

- the header is at `artifacts/v2/<header sha256>/header`; its SHA-256 is the
  digest the cohort's result names (`ResultEntrant::artifact`), and its derived
  inference content is the content the entrant's commitment binds;
- each file is at `blobs/sha256/<file sha256>`, stored once however many
  artifacts list it (the pinned tokenizer, for example).

The files of an artifact are copied before its header, so a published header
names files that are already public. The copies come from the private
admission bucket, where miners upload the header and each file directly with
presigned URLs and R2 verifies each against its committed SHA-256 as it
arrives. Publication hashes nothing: the publisher compares R2's stored
checksums and sizes with the header, and each public copy is checked the same
way: the default server-side `CopyObject` (metadata replaced) keeps the
source's etag but not its stored checksum, so it is checked by its size and by
etag equality with the admitted source, whose SHA-256 R2 verified at upload;
the fallback, a binding stream, is verified by R2 against the digest and
checked by its stored SHA-256. To check an artifact yourself, hash the header
you downloaded, then each file against the digest the header lists.

The publisher withholds, for good and before anything is copied, an artifact
that:

- belongs to an operator or synthetic hotkey;
- has a listed reference digest (header or content);
- is an initial reference (`R_0` or its single replacement) that any
  manifest named, even after the manifest chain stopped naming it (the index
  lists them in `initial_references`). References are published as reference
  packages instead, never as entrant artifacts.

Scores do not exist at scoring start, so no score-dependent rule (the
public-copy gate, near-copy clusters) decides publication. The publisher
never withdraws a published object; only a recorded legal removal takes one
down ([Legal removals](#legal-removals)).

`v1/cohorts/<label>/artifacts.json` (`slop_ninja.public.artifacts/v1`)
records publication at scoring start, one entry per roster entry in roster
order: `uid`, `hotkey` (SS58), `commitment`, `state`, `header` (the header's
SHA-256), `path` (the header key), `reason`, and `files` (each `sha256`,
`bytes`, `path` and `check`: `sha256` when the public copy carries R2's
checksum, `etag` when only its etag ties it to the R2-verified source). The
top level names the `cohort`, `label`, `publisher` build and `copy` mode
(`stream` or `server`).

After the cohort closes, each entrant's artifact has one of three states in
`publication.json`, in `leaderboard.json` and as counts in the index entry,
taken from the scoring-start record:

| State | Meaning |
| --- | --- |
| `published` | Published at scoring start, at `path` (the header key), and its header is the one the result names by digest. |
| `pending` | Not published yet, with a reason: an upload not in the admission bucket, a store that did not answer, the run's wall budget, publication at scoring start not enabled, or an integrity fault (alerted) such as a header the result does not name. Later runs settle it again. |
| `withheld` | Never published, with the reason from the list above. |

An artifact entry also carries the entrant's 64-hex hotkey, registration
block, commitment and opening nonce, as the signed result and closure state
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

### Legal removals

A published object is taken down only by a recorded legal removal
([terms](terms.md#legal-notices-and-removals)), an operator action outside
the publisher, which never withdraws an object itself. The operator deletes
the covered objects from the public bucket (an artifact's header, the blobs
only it lists, or a reference package's directory), keeps every blob that
other published material still lists, purges the deleted paths from the
host's edge cache, and adds an entry to the [removal record](removals.md).
Everything else stays as it was: `artifacts.json`, `publication.json` and
the leaderboard keep the entry and its digests, with the state `published`
recording publication at scoring start; the index keeps its counts; the
cohort summary and the signed objects are unchanged; and a removed path
answers not found. A removal changes no score, result, row or payment.

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
`GET /admission/v2/cohort` is authoritative for uploads. The fields that the
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
was abandoned holds later ones until something shows that no closure
follows: the coordinator's signed abandonment record for it, which releases
it at once, or its retirement marker for it (due after two later
completions).
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
[Rules](#rules)). Nothing else is deleted, apart from a recorded legal
removal ([Legal removals](#legal-removals)). A validator built from this
tree reads no receipt.

## The leaderboard: `v1/leaderboard.json`

Schema `slop_ninja.public.leaderboard/v4`: the newest listed cohort's
[summary](#the-cohort-summary-summaryjson), each entrant with its artifact's
state, plus the target and reference pointers after the close. It carries
nothing computed from the hidden evaluation beyond the summary's quantized
values: no exact score, rank, credit, units, count or distance. The rows
are in roster order, not in rank order.

```json
{
  "schema": "slop_ninja.public.leaderboard/v4",
  "network": "testnet",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "cohort": 12, "label": "testnet-c12", "closed_epoch": 23120,
  "quantization": { "rule": "quant-v1", "width": "0.03" },
  "references": {
    "index": 1,
    "current": { "content": "<64 hex>", "artifact": "<64 hex>", "q": "0.84" },
    "predecessor": { "content": "<64 hex>", "artifact": "<64 hex>", "q": "0.81" }
  },
  "promoted": "<ss58>",
  "promotion_rule": { "lookup_canary": false, "scope": "testnet-only", "note": "…" },
  "active_target": { "digest": "<64 hex>", "source": "cohort", "cohort": 12 },
  "current_reference": { "schema": "slop_ninja.public.reference/v1", "role": "current", "…": "…" },
  "previous_reference": { "schema": "slop_ninja.public.reference/v1", "role": "previous", "…": "…" },
  "entrants": [
    { "uid": 7, "hotkey": "<ss58>", "hotkey_hex": "<64 hex>", "q": "0.90", "band": 1,
      "gate": "pass", "copy_gate": "pass", "cluster": { "uid": 7, "hotkey": "<ss58>" },
      "artifact": { "uid": 7, "hotkey": "<ss58>", "sha256": "<64 hex>", "content": "<64 hex>",
                    "state": "published", "path": "artifacts/v2/<sha256>/header", "reason": null,
                    "hotkey_hex": "<64 hex>", "registration_block": 4100,
                    "commitment": "<64 hex>", "nonce": "<64 hex>" } }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `network`, `chain`, `cohort`, `label`, `closed_epoch` | As in the summary |
| `quantization`, `references` | The summary's grid and `R_k`'s and `R_k-1`'s digests and `q`, as the cohort's manifest named them |
| `promoted` | The entrant promoted to `R_k+1` (SS58), or `null` |
| `promotion_rule` | Testnet records only (absent for any other chain): whether promotions run the lookup canary (`lookup_canary`, `false`), its `scope` (`testnet-only`) and a `note`, which the page shows. Testnet promotions run without the lookup canary (decision D12b), though an operator drill named in the cohort's plan can still block a candidate; the canary is required before mainnet |
| `active_target` | The public index's `active_target` when this cohort was listed (see the cohort index), a record pointer rather than live validator state: its `digest`, its `source` (`launch_burn` or `cohort`) and the `cohort` whose closure activated it (`null` for the launch burn) |
| `current_reference`, `previous_reference` | The `R_k` and `R_k-1` pointers after this close, the same documents as `v1/reference/current.json` and `previous.json` |
| `entrants[]` | Every entrant in roster order: the summary's fields (`uid`, `hotkey`, `hotkey_hex`, `q`, `band`, `gate`, `copy_gate`, `cluster`, with the summary's meanings) and `artifact` |
| `entrants[].artifact` | The entrant's entry in `publication.json`: `uid`, `hotkey`, `sha256` (the artifact digest the result names, the header's SHA-256), `content`, `state` (`published`, `pending` or `withheld`, [above](#artifacts-artifactsv2sha256header-and-blobssha256sha256)), `path` (the header key when published), `reason`, and the `hotkey_hex`, `registration_block`, `commitment` and opening `nonce` the signed result and closure state; `null` when the publication has no entry for the hotkey |

The cutoff and band edges are only in the summary (`quality`). The q-rank
(`q` descending, then roster order) follows from the published values;
no field states it.

The publisher writes the document when the newest cohort is listed, and
rewrites it when a later run settles one of that cohort's pending artifacts
(the rows' `artifact` and a reference pointer's download or package link
are refreshed then). A record restart deletes it with the reference
pointers.

The page (`index.html`, with `style.css`) is rendered from the same
document at the same times: the cohort and label, the closing epoch, the
promoted hotkey, the active target, the current reference with its
download or reference-package link, and the promotion-rule note; then one
row per entrant in roster order with its UID, hotkey, gate, copy gate,
cluster (by its first member's UID), band and artifact link or state. It
has no score column. Before any cohort is listed, and after a record
restart, the page says no cohort has closed yet and shows the testnet
promotion-rule note.

## The public status API: `slop.ninja/api/`

The status page's tracker links to a read-only JSON API for the cohort it
shows. The website Worker answers it from the checked `dashboard/v1`
documents it already reads through the private dashboard feed
([`dashboard-contract.md`](dashboard-contract.md)), never from the
coordinator, the validator layout `v1/` or any private bucket. Validators
never read it. The API is versioned by path. This section is its
contract. **State:** being rolled out with the website. The miner list
(`miners`) stays incomplete until the registry and roster documents below
are written.

| Path | Schema | What |
| --- | --- | --- |
| `GET /api/` | `slop_ninja.api.discovery/v1` | endpoints, networks, mechanisms and their states, schema links |
| `GET /api/v1/<network>/<mechanism>/status` | `slop_ninja.api.cohort_status/v1` | the current cohort (`view: "current"`), or the next one when none runs |
| `GET /api/v1/<network>/<mechanism>/cohorts` | `slop_ninja.api.cohorts/v1` | the listed cohorts, plus the current one |
| `GET /api/v1/<network>/<mechanism>/cohorts/<label>` | `slop_ninja.api.cohort_status/v1` | one cohort (`view: "cohort"`); the current cohort's label answers like `status` |
| `GET /api/v1/schemas/<name>.json` | JSON Schema 2020-12 | `cohort_status`, `cohorts`, `discovery`, `error` |

`<network>` is `testnet`, `<mechanism>` `a` (a mechanism that is not live
answers 404 `mechanism_not_live`) and `<label>` a cohort label. `GET`,
`HEAD` and `OPTIONS` only; every answer, errors included, carries
`Access-Control-Allow-Origin: *` and no credentials. Query strings are
ignored. Answers are cached like the feed: 60 s for `status`, `cohorts` and
the current cohort, 300 s for a closed cohort, 3,600 s for a schema. Errors
are `slop_ninja.api.error/v1` documents (`{"available": false, "error":
{"code", "message"}}`) with the codes `not_found`, `mechanism_not_live`,
`not_listed` (404), `method_not_allowed` (405), `not_enabled`,
`upstream_unavailable` and `upstream_invalid` (503). Within `v1` fields can
be added, never renamed, removed or given another meaning; an unknown
value is `null`, and no key is ever omitted.

A `cohort_status` answer carries the network and chain, the mechanism, the
sources it was composed from with their `generated_at`, the clock and the
pause state, and `cohort`: number, label, state, stage (with its kind and
position on the eight-stage bar), mode, tempos credited and required,
admission progress (frozen count and roster cap), the work percent on the
10% grid, the phase history, the plan, the bounds, and `expected` (the
current stage's earliest end and the earliest close). Every future
boundary carries its epoch and block and an estimate `at_estimate` (unix
seconds) and `at_estimate_utc` (RFC 3339) under rule
`slop_ninja.api.eta/v1`: the coordinator's observation time plus the
blocks to go at the nominal 12 s block, floored to the minute, `null` once
the boundary is due or passed. **Estimates are lower bounds**: the chain's
block time varies, a coordinator outage or pause stops tempo credit and slow
work moves a stage to a later boundary, so a stage is never earlier than its
boundary and may be later. The answer also carries `next` (the next cohort's
earliest opening), `references` (`R_k` and `R_k-1` by index and label, with
their digests once the roster documents carry them), `outcome` (a closed
cohort's promotion and entrant count), `coordinator_weights` (the
coordinator's latest commit-reveal weight commit, public chain facts) and
`miners`.

`miners` lists hotkeys in ascending UID order with `source` (where the list
comes from), `complete` (`true` only when it is every registered UID),
`as_of` (the boundary it describes) and `rows[{uid, hotkey, hotkey_hex,
registration_block, registered, cohort_status, reason_class,
roster_position, result, artifact_state, challenges}]`. `cohort_status` is one of the
roster statuses below; `challenges` is an admitted entrant's challenge
counts while its cohort runs (below), else `null`. Until the registry is written the list is the UID
map of committing hotkeys (`complete: false`) or `null`; until the rosters
are written a running cohort's rows carry no status, and a closed cohort's
rows come from its cohort document (gate pass or fail only).

What the API never serves: held-out text, labels, per-document predictions,
receipts, customer data, exact scores, and (in `v1`) no score at all: no
`q`, cutoff, band edge, row share or eligibility, before or after close; no
rank or score order, and no result or roster position before the cohort
closes. After close it serves only what the cohort summary publishes: bands,
gate bits, cluster membership and roster order. The one per-entrant work
figure is each admitted entrant's challenge counts while its cohort runs:
how many of its challenges are scored, of its total, and nothing else (no
score, leader, order, challenge identity, failure or retry detail; the
residual is in the [dashboard contract](dashboard-contract.md)).

### Its dashboard documents: `registry.json` and `rosters/<label>.json`

Two `dashboard/v1` documents (bounds rule
`slop_ninja.dashboard.read_bounds/v3`) back the miner list. The publisher
writes them under the same allow-list, redaction scan and bound checks as
every dashboard document (`subnet/publisher/src/dashboard_roster.rs`); the
feed serves them, and the website is to check them again before the API
uses them. A missing document leaves the API's earlier values.

`dashboard/v1/registry.json`, schema `slop_ninja.dashboard.registry/v1`,
bound 64 KiB + 256 rows of 198 bytes, cache 60 s:

```json
{ "schema": "slop_ninja.dashboard.registry/v1", "network": "testnet",
  "chain": { "genesis": "0x…", "netuid": 601 }, "generated_at": 1793951220, "sample": false,
  "rule": "slop_ninja.coordinator.registry/v1", "epoch": 145, "block": 8384078,
  "rows": [ { "uid": 0, "hotkey": "<ss58>", "hotkey_hex": "<64 hex>", "registration_block": 4100 } ] }
```

Every registered UID of the subnet, ascending, each hotkey once with both
spellings, and its registration block: public chain facts, as the
coordinator's latest signing operation read them (`Keys` and
`BlockAtRegistration` for every UID below `SubnetworkN`, at the confirmed
`block` of `epoch`). It is fresh to one tempo: a signing pause leaves the
last read, which `epoch` and `block` date. Written each run from a
coordinator that reports the read; `epoch`, `block` and `rows` are all
`null` while no whole read is available (none yet, more than 256 UIDs, or a
malformed read).

`dashboard/v1/m/<key>/rosters/<label>.json`, schema
`slop_ninja.dashboard.roster/v1`, bound 64 KiB + 512 rows of 1 KiB (every
registered UID plus every former entrant), cache 60 s:

```json
{ "schema": "slop_ninja.dashboard.roster/v1", "network": "testnet",
  "chain": { "genesis": "0x…", "netuid": 601 }, "generated_at": 1793951220, "sample": false,
  "mechanism": "a",
  "cohort": { "cohort": 18, "label": "testnet-r0-18", "stage": "closed" },
  "level": "closed", "source": "summary", "as_of": null,
  "rows": [ { "uid": 7, "hotkey": "<ss58>", "hotkey_hex": "<64 hex>", "status": "scored",
              "reason_class": null, "roster_position": 2,
              "result": { "gate": "pass", "copy_gate": "pass", "band": 1,
                          "cluster": { "uid": 7, "hotkey": "<ss58>" } },
              "artifact_state": "published", "challenges": null } ],
  "references": { "index": 0, "current": { "content": "<64 hex>", "artifact": "<64 hex>" },
                  "predecessor": null } }
```

| Field | Meaning |
| --- | --- |
| `cohort.stage` | The bar stage `now.json` showed when the roster was written (read at tempo boundaries), or `closed` |
| `level` | How much of a running cohort the publisher publishes (below); `closed` on a closed cohort's roster |
| `source` | `summary` (a closed cohort's public summary), `admission` (the eval Worker's per-hotkey rows) or `artifacts` (the published artifacts record, from scoring start) |
| `as_of` | The tempo boundary `{epoch, block}` the eval Worker's rows describe; `null` otherwise |
| `rows[]` | One per hotkey, in ascending UID order (rows without a UID last), then by hotkey; never roster or score order |
| `rows[].status` | `none`, `committed`, `uploading`, `verifying`, `verified`, `refused`, `overflow`, `admitted`, `evaluating` (running cohort), or `scored`, `withheld`, `not_executed` (closed cohort) |
| `rows[].reason_class` | Only with `refused`: `commitment`, `artifact`, `cutoff` or `registration`, never a refusal code or detail |
| `rows[].roster_position`, `rows[].result` | Closed cohorts only: the 1-based roster order and the summary's `gate` (`pass`, `fail`, `withheld`, `not_executed`), `copy_gate`, `band` and `cluster`. No `q` |
| `rows[].artifact_state` | `published`, `pending` or `withheld`, from scoring start |
| `rows[].challenges` | `admitted` and `evaluating` rows of a running roster at the `sessions` level only: `{state, done, total}`, `pending` before the entrant's scoring starts (`done` 0, `total` `null` until the batch is sealed), `running` while `done < total`, `complete` at it; `null` otherwise, and when the eval Worker reports none (an entrant whose content was already published is not scored and has none). Read each run |
| `rows[].live` | `admitted` and `evaluating` rows of a running roster at the `live` level only (absent otherwise): `{checkpoints, stopped, prefinal}`, the entrant's `live-v1` segment readings (each a multiple of `live.width` with exactly its decimals, or `withheld`; at most `live.released_max`, released once), whether more attributable failures than the minimum-success limit stopped them, and its pre-finalized quantized score (`quant-v1` grid, or `withheld`; `null` until it and `R_k` finish) |
| `references` | Closed cohorts only: `R_k` and `R_k-1` by content and artifact digest, from the summary |
| `live` | A running roster at the `live` level only (absent otherwise): `{rule, width, segments, released_max, noise_sd, eps_live_milli, bits{counted, live, bound}, reference{done, total, checkpoints, stopped}, prefinal{quantization{rule, width}, current, predecessor, margin_line}}`. `reference.done` moves in segment steps; `prefinal` is `null` until `R_k` finishes without a failure |

A closed, listed cohort's roster is built from its summary and publication
record alone and rewritten with its cohort document. It lists the
entrants only. The non-admitted hotkeys' frozen statuses are not kept
after close. `scored` covers the gate bits `pass` and `fail`.

A running cohort's roster is written each run at the publisher's
`DASHBOARD_ROSTER` level; each level includes the ones before it. Testnet
runs at `sessions`: publishing each miner's intake state while a cohort
runs was approved on 2026-11-06. No level below `live` carries a score; no
level carries a rank, roster position, gate, band or cluster before the
cohort closes. A roster is written at `live` only when it carries the live
block; otherwise at `sessions`.

| Level | A running cohort's roster |
| --- | --- |
| `closed` (the code's default when unset) | none: closed cohorts only |
| `scoring` | from scoring start, each entrant `evaluating` with its artifact state, from the published artifacts record |
| `frozen` | the eval Worker's per-hotkey rows: before the roster freezes only `none` or `committed`, from the logged on-chain commitment alone (a session without one reads `none`); from the freeze (once the page has left the admission window) `admitted`, `evaluating` from `generating` on, `refused` with its class, or `overflow` |
| `sessions` | before the freeze also `uploading`, `verifying`, `verified` and `refused` with its class; from the freeze each admitted entrant's challenge counts (selected on 2026-11-06) |
| `live` | also the running cohort's live values from the eval Worker's `/eval/live` (rule `slop_ninja.eval.live_scores/v1`): `live-v1` segment readings and pre-finalized quantized scores (`rows[].live`, `live`); the eval Worker's operator stop removes them at the next run |

`admitted` and `evaluating` come from the frozen roster and the stage the
page shows, never from per-entrant work; only `challenges` does. A running roster is no longer
rewritten once its cohort leaves the running stages; the closed roster
replaces it at the same key when the cohort is listed. It is also not
rewritten when its source disappears, for example when the eval Worker stops
reporting a frozen cohort's hotkeys during `generating` or `sealing`, before
the artifacts record exists; it can then lag the stage the page shows, and
its `cohort.stage` and `as_of` date it.

### Their inputs: the coordinator's registry and the eval Worker's hotkeys

Both documents come from content-free status fields:

- the coordinator's `/status` `chain_journal.registry{rule:
  "slop_ninja.coordinator.registry/v1", epoch, block, rows[{uid, hotkey (64
  hex), registration_block}]}`: the latest registration read of a signing
  step (the coordinator's own commitment or the row snapshot other
  validators copy, once a tempo in every signer state), kept in the
  journal (schema v14, one row, never replaced by an earlier block)
  instead of dropped; rows ascending by UID; more than 256 UIDs answers
  `rows: null, refused: "over_bound"`; `null` before any read;
- the eval Worker's `/eval/status` `admission.cohorts[].hotkeys{rule:
  "slop_ninja.eval.admission_hotkeys/v1", cohort, epoch, block,
  rows[{hotkey (64 hex), uid, committed_block, state, refusal_class,
  roster_position}]}` for one cohort, the newest admitting or closing one,
  else the newest frozen one while its evaluation runs (not closed or
  abandoned; one report keeps the status inside its arena footprint): each hotkey of the UID map at the stored boundary and
  each hotkey with a session of the cohort, at most 512 rows, refused whole
  (`rows: null` with `refused`) rather than truncated. `committed_block` is
  the block of the hotkey's counted commitment (its latest while the window
  admits, its last before the cutoff once closed) when that commitment was
  made inside the admission window; the commitment value is a digest, so
  which cohort it opens shows only through a session. `state` is the latest
  session's state (a superseded session is replaced by the later one), else
  `committed` or `none`; `refusal_class` the closed class of the session's
  refusal (`commitment`, `artifact`, `cutoff`, `registration`), mapped
  inside the eval Worker (a refusal no class covers refuses the whole
  report); `roster_position` (admitted rows from the freeze, commitment
  order) is informational and never published: the publisher drops it, and
  the closed roster takes its order from the cohort summary. A session
  hotkey outside the UID map takes its commitment's UID unless a map row
  holds it. No session id,
  digest, object key, size, mint count, timestamp or verdict try leaves the
  eval Worker;
- on that frozen cohort, `admission.cohorts[].progress{rule:
  "slop_ninja.eval.scoring_progress/v1", cohort, sealed, rows[{hotkey (64
  hex), started, done, total}]}`: `sealed` once the cohort's jobs exist;
  one row per entrant job, in hotkey order (never roster order), at most
  256 (`MAX_ROSTER`), refused whole (`rows: null` with `refused`); `done`
  the documents of its committed chunks (a chunk commits whole, so `done`
  moves in steps of `scorer.chunk_docs`, and a model error counts as
  resolved), `total` all its chunks' documents, `started` its first claim.
  No prediction, outcome, score, job or chunk number, lane, attempt, crash,
  fault, timing or roster position leaves the eval Worker. The publisher
  reads it whole (the rule, the cohort, at most 256 rows in ascending
  hotkey order, `1 <= total`, `done <= total`, nothing done before
  `started`, no row before `sealed`) and publishes `pending` (not started),
  `running` or `complete` with the two counts.

The publisher reads each whole or not at all (the closed vocabularies,
ascending distinct UIDs, distinct hotkeys, the bounds); a malformed field
never fails the rest of the status.

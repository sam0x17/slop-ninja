# Admission API (testnet, contract `slop_ninja.admission/v2`)

This is the contract between the coordinator's admission routes and the miner
client `slop_ninja_miner` (`subnet/crates/miner`). It covers the on-chain
commitment and how the coordinator reads it, the hotkey-signed sessions, the
direct upload of every artifact object to the coordinator's private R2 bucket
under presigned URLs, the admission cutoff, the one-artifact-per-hotkey rule,
the status endpoints and the error codes. It applies to testnet subnet 601.

**Two rules shape version 2** (user direction, 2026-11-04):

- **Every artifact is hashed once.** The miner hashes each file once, at
  `pack`, to build the header the commitment binds. R2 verifies each uploaded
  object against that committed SHA-256 as the bytes arrive and stores
  nothing on a mismatch. No Slop Ninja Worker, Durable Object or Container
  hashes a model byte: admission compares R2's stored checksum and size with
  the header, and every later reader takes the header's digests.
- **No upload byte passes through any Slop Ninja Worker.** The header and
  each file go straight to R2 as one presigned `PutObject` each. The public
  upload Worker takes only signed session requests (each at most one
  `SubmissionUpload` record) and holds no key and no R2 binding.

It is built on the protocol crate's submission types in
`subnet/crates/protocol/src/submission.rs` and the artifact object in
`subnet/crates/protocol/src/artifact.rs`: `SubmissionOpening`,
`SubmissionUpload` (record version 2), `FinalizedCommitment`, `RosterEntry` and
`VerifiedHeader`. The artifact checks are those of the *A* artifact spec
(`docs/testnet/artifact-spec.md`, crate `slop_ninja_artifact_check`).

The byte-level helpers below (request message, route parser, session id,
error codes, response limit) are implemented once in the crate
`slop-ninja-admission-wire` (`subnet/crates/miner/wire`). The presigned URL
form, the object keys and the URL policy are in `slop-ninja-r2-presign`
(`subnet/crates/r2-presign`). Neither performs I/O or keeps global state; their
arena-safe items allocate nothing and build for `wasm32-unknown-unknown`.

Status (2026-11-04): this text is the version 2 contract. The wire crate, the
front gate (`slop_ninja_admission_gate`), the presigner and the internal signer
Worker (`subnet/r2-signer`) implement it; the cohort authority, the upload
Worker's v2 routes and the miner client's direct upload are being rebuilt on
it (branch `subnet-testnet/direct-r2-upload`), and nothing of version 2 is
deployed. Three Workers take part:

- the **upload Worker** `slop-ninja-admission-testnet`
  (`subnet/admission-worker`), served at `https://testnet-admission.slop.ninja`,
  takes every public request. It holds no key and no R2 binding. It checks each
  request (signature, registration on the subnet, body bounds) before anything
  else, and decodes uploaded headers for the cohort authority on an internal
  route;
- the **eval Worker** (`subnet/coordinator/src/eval/admission.rs`,
  `commitments.rs`, `calendar.rs` and `src/cf_admission.rs`) holds the package
  and receipt keys and the cohort state. It serves no public admission route;
- the **signer Worker** `slop-ninja-r2-signer-testnet` (`subnet/r2-signer`)
  holds the one R2 S3 credential, scoped to the admission bucket, and presigns
  the uploads. It has no route and no `workers.dev` name; only the eval Worker
  can call it, through a service binding.

Every value under "Parameters" is announced by the coordinator rather than
assumed by the client: each is a parameter of the cohort's frozen plan (its
`admission` block), derived from the plan's artifact spec, or the versioned
upload URL policy.

## 1. Flow

1. `next-cohort` reads the public status snapshot with `--public` (section
   10); otherwise the coordinator's unsigned cohort read (`GET
   /admission/v2/cohort`). It reports the admitting cohort, or the next one
   and its earliest opening epoch.
2. `pack --dir MODEL_DIR` builds the artifact object `u32_le(len(H)) || H ||
   files` (`artifact.rs`) from the artifact spec's six files, hashing each file
   once while it writes it. `check` runs the spec's admission checks on the
   object in place, hashing nothing.
3. `commit` reads only the object's header, makes a fresh secret 256-bit nonce
   and the opening for the admitting cohort, stores the opening in a local file
   with mode 0600, and signs `Commitments.set_commitment` with the hotkey
   (section 2).
4. After the commitment finalizes and the coordinator has logged it, `upload`
   opens a session at an authenticated coordinator origin with the envelope
   (the opening, the commitment and the header's SHA-256 and length), PUTs the
   header bytes straight to R2 with the URL the session returns, asks for the
   file URLs (the coordinator checks the header first), PUTs each file straight
   to R2, and completes the session (sections 5 and 6). An interrupted upload
   resumes per file: the session lists which objects are recorded present.
5. The coordinator records each object from R2's stored checksum and size,
   runs the scorer's admission load on the files, ranks the remaining entrants
   at the cutoff and freezes the roster (section 7). `status` shows the result,
   including every refusal (section 5, the hotkey view).

## 2. The commitment

The opening is the protocol's `SubmissionOpening`: the chain binding (genesis
hash, netuid, mechanism), the cohort, the hotkey, its registration block
(`BlockAtRegistration(netuid, uid)`), the canonical inference-content digest
derived from the artifact header, and a fresh secret nonce. The committed digest
is `SubmissionOpening::commitment()`.

The chain payload is the 44 bytes `SubmissionOpening::chain_payload()`:

```
"sna1" || u64_le(cohort) || commitment digest (32 bytes)
```

The call is

```
Commitments.set_commitment(netuid, CommitmentInfo { fields: [Raw44(payload)] })
```

signed by the miner's hotkey. The field is the length-indexed raw variant
(`Data::Raw` with 44 bytes, which metadata names `Raw44`); no hash variant is
used, because the coordinator and validators must read back the exact 44 bytes
(`FinalizedCommitment::payload`). For netuid 601 under the recorded spec-486
metadata the call bytes are

```
12 00 | 59 02 | 04 | 2d | <44-byte payload>
```

(pallet 18, call 0, `u16` netuid, one field, variant index 45). Production code
resolves the call by name in whatever metadata the chain serves; these bytes are
a test vector, not a runtime gate. The miner's golden test
(`subnet/crates/miner/tests/golden.rs`) checks the payload against the protocol
fixture line `submission.chain_payload` in
`subnet/crates/protocol/fixtures/golden-reward-v1.txt` and pins the call bytes.

The chain charges no fee for `set_commitment`. The client sends it as a raw
call because rtao has no typed intent for it; rtao applies no spend or netuid
limit to raw calls, so the client composes the call itself and pins it with
`expected_call`, and rtao signs exactly those bytes.

Rules:

- **One cohort.** An opening names exactly one cohort, and the payload repeats
  it. `SubmissionUpload::admit` refuses any other cohort. A commitment can never
  be carried to a later cohort.
- **Fresh nonce.** Every commitment uses a new nonce from the operating system's
  random source. The client never reuses an opening, and a dry run stores none.
- **Last commitment before the cutoff wins.** The chain keeps one commitment
  per `(netuid, hotkey)` (`CommitmentOf`); a new `set_commitment` overwrites
  it. For admission the coordinator uses the hotkey's last logged commitment
  before the cutoff block (below). An upload that opens an earlier, overwritten
  commitment is refused.
- **Commitment window.** A commitment counts for cohort `N` only if its block
  is at or after `N`'s opening block and before its cutoff block (section 3).
  A commitment made before the cohort opened is refused with
  `commitment_before_open`, so no one can reserve a place in a later cohort's
  order ahead of time. A commitment made at or after the cutoff block does not
  count for `N`, even when it is already finalized when the coordinator closes
  admission.
- **Position.** The admission position is the commitment's finalized block and
  the index of the extrinsic that emitted `Commitments.Commitment { netuid, who }`
  for the hotkey in that block (`RosterEntry::position`). If the hotkey
  committed twice in one block, the later extrinsic is the one storage holds and
  the one that counts.
- **Registration generation.** The committing hotkey's registration (UID and
  registration block) is read at the commitment block. It must equal the
  opening's, and the hotkey must have registered no later than the commitment
  block. A hotkey that is pruned and registers again starts a new generation and
  needs a new commitment.
- **Chain limits.** Each `set_commitment` uses at least 100 bytes of the
  account's per-epoch commitment space (`UsedSpaceOf` against `MaxSpace`), and a
  call that exceeds it fails with `SpaceLimitExceeded`. The deposit and the space
  limit are the chain's, read live; the client assumes neither.

### Reading commitments: the commitment log

The chain holds only each hotkey's latest commitment, and the hotkey replaces
it whenever it commits again, including for a later cohort. Ordinary RPC nodes
also keep state for recent blocks only. So admission never reads a past
commitment from current state. The coordinator keeps a durable **commitment
log** on its RPC-confirmed watermark, read from its cron tick, every call
reserved in the shared RPC budget first:

- **Poll.** While a cohort admits, at most once a minute, it lists the
  subnet's `CommitmentOf(netuid, *)` keys at the confirmed block and reads
  their values with `System.LastRuntimeUpgrade` in one call. A
  `CommitmentOf` value carries the block that set it, so a changed
  `(block, payload)` is a new commitment.
- **Resolve.** For each new commitment at block `c` it reads `c`'s hash, then
  `System.Events` and `Uids(netuid, who)` at `c`, then
  `BlockAtRegistration(netuid, uid)` at `c`, and logs `who`, the block and
  hash, the payload, the extrinsic index of the last
  `Commitments.Commitment { netuid, who }` event for the hotkey in `c`, and
  the hotkey's registration at `c`. These reads normally come within minutes
  of the commitment. State the RPC endpoint reports discarded is read again
  from the archive endpoint when one is configured; if every configured
  endpoint reports `c`'s state discarded, the commitment is unresolvable
  (`commitment_unresolvable`, `state_discarded`). An answer that serves no
  state without saying it is discarded (a lagging or load-balanced node) is
  never final: the read is repeated after a growing backoff. The coordinator
  never substitutes a later state for an earlier block.
- **Snapshot.** When admission closes, for every hotkey whose upload
  completed in time, it reads `CommitmentOf`, `Uids`, `Owner` (the owning
  coldkey) and then `BlockAtRegistration` at the hash of `cutoff_block - 1`: the state the
  last block before the cutoff left. That is exactly the hotkey's last
  commitment before the cutoff block and the registration that held then.
  The roster does not freeze before the snapshot is read.
- Opening a session uses the hotkey's latest logged commitment; while a newer
  one the poll saw is still being resolved, opening answers
  `commitment_not_found` (retryable). The close uses the snapshot.
- A hotkey that commits again (a returning miner, or a replaced commitment)
  can open a session before the poll has seen the new commitment. While a
  cohort admits, opening a session for that cohort whose logged
  commitment is older than the window, opens another opening or another
  registration generation, or held no UID is answered `commitment_not_found`
  (retryable) until the reader has completed a poll at or after a block fixed
  at the first such answer for that opening: the later of the finalized
  cursor then and the request's `x-sn-commitment-block` (the miner's own
  confirmed commitment block, an untrusted hint; a later request with a
  higher one moves the block up), or without that header, the later of the
  cursor and one past the reader's last completed poll then. The wait is tied to that fixed block, not to the cursor, which
  advances every block while the reader polls less often; so
  `commitment_before_open`, `commitment_mismatch`, `registration_mismatch`
  and `not_registered` become final at the reader's next completed poll at
  or after that block. With the hint, a coordinator whose cursor lags the
  block holding the new commitment (a stalled heartbeat or webhook) keeps
  answering `commitment_not_found` rather than a refusal based on the older
  commitment. The `Retry-After` is the reader's shortest poll interval, the
  earliest a new poll can complete, not a promise: a slower poll or a stalled
  cursor answers the same code again. A false hint delays only that hotkey's
  own answer, never past the window's close. `wrong_chain` never waits.
  Verification (after the session opened against the logged commitment)
  never waits: a commitment replaced after the open refuses the session with
  its real code.
- A commitment that a later one replaced before a poll saw it is never
  logged. It counts for no cohort: only the latest commitment counts while a
  cohort admits, and the last one before the cutoff block at the close, which
  the snapshot reads exactly.
- The reads scale with commitments, not sessions: about two calls a minute
  while a cohort admits, four per new commitment, and three per verified
  hotkey at the close. Storage decodes by name from the runtime's own V15
  metadata, kept beside the log and read again whenever
  `LastRuntimeUpgrade` reports another spec version.

Validators check a roster entry against the commitment at its finalized
position block (`CommitmentOf` at that block's hash, or the `set_commitment`
call in that block's body), never against their latest state: an entrant of
cohort `N` usually commits again for `N + 1` before `N` is checked. The
protocol's `check_entrant` takes the `FinalizedCommitment` the validator reads,
so the read height is the validator's to get right.

## 3. Cohort, window and cutoff

Times are counted in the subnet's finalized epoch index, as the protocol
counts them (`CohortManifestV2::opened_epoch`, `CohortSchedule`).

- **Opening block.** The first block whose epoch index for the subnet is the
  cohort's `opened_epoch`.
- **Planned cutoff epoch.** `opened_epoch + admission_min`. This is the deadline
  a miner plans for.
- **Cutoff block.** The first block of the epoch at which the coordinator
  closes admission. Admission closes at the first eligible boundary at or
  after the planned cutoff epoch at which the coordinator is running. During
  a coordinator outage the cutoff moves to a later boundary; it never moves
  earlier, and the cohort is never skipped. The hotkey view reports it once
  admission has closed.
- **Freeze.** The roster freezes at the close or later, once every upload
  completed before the close is verified or refused, every admission verdict
  is in and the commitment snapshot is read; the cohort waits until then and
  is never skipped. The closure's `admission_closed_epoch` is the epoch of
  the freeze.
- **Commitments** count when their block is in `[opening block, cutoff block)`,
  as the commitment log records them.
- **Uploads** count when the coordinator recorded the header and every file
  of the session present and matching before it recorded the close. Once the
  close begins (`closing`), only the close's own sweep records presence: it
  heads every unrecorded object of every open session once, and an object
  present at its close-sweep head counts. A write that lands after its key's
  close head does not count. Verification that was accepted before the close
  finishes after it and still counts.

A commitment needs finality and the coordinator's logging of it (the nominal
wait is at most 60 seconds after finality, and it can be longer) before a
session can open. The objects, up to about 955 MB together (the spec's
largest artifact), must then be uploaded before the cutoff. Commit and upload together,
not near the cutoff. The window is the standing template's `admission_tempos`
(16 tempos under the testnet template), frozen in each cohort's plan when it
is queued, so a cohort queued before the template took that value keeps its
own (one tempo, or 17); `next` in the cohort response and the status snapshot
gives the next cohort's. The operator's calendar view reports the template's value, the
last window as measured on chain (its blocks per tempo taken over the epoch
boundaries it spanned) and the template's window at that interval, in blocks
and nominal minutes.

## 4. Signed requests

Every coordinator request except `GET /admission/v2/cohort` is signed by the
hotkey that the request concerns. There are no accounts, tokens or cookies.
The only other requests are the presigned PUTs to R2 (section 6), which carry
no hotkey signature: their URL is the authorization, minted for the session's
owner only.

Headers:

| Header | Value |
| --- | --- |
| `x-sn-hotkey` | SS58 address (prefix 42) of the signing hotkey |
| `x-sn-valid-until` | decimal finalized block number after which the request is refused |
| `x-sn-content-sha256` | lowercase hex SHA-256 of the request body (of the empty string when there is none) |
| `x-sn-signature` | lowercase hex of the 64-byte sr25519 signature |
| `x-sn-commitment-block` | optional, session creation only, outside the signature: the decimal block of this wallet's confirmed commitment for the opening (an untrusted hint; see section 2) |

The signed message is 32 bytes:

```
message = SHA-256( u32_le(len(D)) || D || body )
D       = "slop_ninja.admission/request/v1"
body    = genesis (32) || u16_le(netuid) || u8(mechanism)
        || hotkey public key (32)
        || u64_le(valid_until)
        || u32_le(len(method)) || method        e.g. "POST"
        || u32_le(len(path))   || path          e.g. "/admission/v2/sessions/<id>/urls"
        || content sha256 (32)
```

This is the protocol's `domain_hash(D, body)` with the protocol codec's
integer and string conventions. The path is the request path exactly as sent,
without scheme, host or query; no route uses a query string. The hotkey signs
the 32-byte message itself with sr25519 under the Substrate signing context,
with no `<Bytes>` wrapper (rtao `Signer::sign_raw`). The coordinator verifies it
strictly (`rtao_core::keys::verify_strict`, as `verify_sr25519` in
`subnet/coordinator/src/chain.rs` does).

The upload Worker checks every signed request in this order, before it reads
a body or forwards anything:

1. the route parses (`bad_request` otherwise). Every path segment is bounded
   by its format; a hotkey segment by the SS58 shape (at most 50 base58
   characters);
2. the four signature headers are present and the signature verifies
   (`bad_signature`). This is CPU only, with no I/O;
3. in `hotkeys` mode (operator drills), the signer is listed (`not_found`
   otherwise);
4. the header-only checks, still without I/O: a hotkey segment decodes as an
   SS58 account and names the signer (`bad_request`, `forbidden`); a
   creation carries the upload media type (`unsupported_media_type`); a
   bodiless route's signed `x-sn-content-sha256` is the empty string's
   (`content_digest_mismatch`); a declared `Content-Length` is within the
   route's bound (one `SubmissionUpload` record for a creation, none for a
   bodiless route; `bad_request`). No route takes an upload byte;
5. the signer holds a UID on the subnet at the finalized head
   (`not_registered`). A session read and `repair` that fail it (the signer
   is not registered, or the roster cannot be read) go on only when the
   cohort snapshot lists the signer among the hotkeys whose sessions wait
   for a repair (or lists everyone, past one roster of them), so a
   deregistered entrant may still repair its admitted objects while an
   unregistered key's signed requests never reach the coordinator; the
   coordinator still requires the session to be the signer's own and a
   repair's objects to be still needed. This is a lookup in the Worker's roster of the
   subnet's hotkeys, which each isolate reads once a block (12 s) with
   read-only RPC calls (`SubnetworkN` and `Keys` at the head, keys composed
   by name from live metadata, a runtime upgrade followed), so the request
   itself costs no I/O. While the roster cannot be read the Worker answers
   `unavailable` (retry 12 s);
6. only then is the body read, within its route's bound, and compared with
   `x-sn-content-sha256`. A body the platform could not read answers
   `unavailable` (retryable).

The upload Worker does not check `valid_until`: its chain head can run ahead
of the cohort authority's cursor, and a check against the head could refuse
a request the authority accepts. The steps above are one function,
`slop_ninja_admission_gate::decide`, which the miner client's mock also runs.

The cohort authority then refuses a request when:

- the signature does not verify (`bad_signature`; it checks again);
- `valid_until` is below its finalized cursor (`request_expired`, with the
  cursor in `detail.finalized_block` so the client re-signs from it);
- `valid_until` exceeds its cursor by more than `request_validity_blocks`
  (`request_validity_too_long`);
- a body's SHA-256 differs from `x-sn-content-sha256` (`content_digest_mismatch`);
- the hotkey is not the one the path or session belongs to (`forbidden`).

**Replays.** A signature is bound to the chain, the method, the path, the body
digest and `valid_until`, so it cannot serve on another network, route or body,
or after `valid_until`. Inside the validity window an observer who saw a
request could send it again. Each route is safe against that:

- a read (`GET`) changes nothing;
- `urls` and `repair` mint more URLs for the same derived keys, whose content
  R2 fixes to the committed digests;
- a completion records only objects R2 holds, and returns the session once it
  is complete;
- `POST /admission/v2/sessions` returns an existing session in whatever state
  it is.

A presigned URL is a bearer token until it expires, but it can only create its
one key with exactly the committed bytes (R2 refuses any other body), and only
once (`if-none-match: *`): a URL replayed after success gets 412.

**Destination.** An upload carries the opening, with its secret nonce, and the
artifact before the cohort closes, so the client sends signed requests only to
an authenticated origin:

- an origin listed in `admission_origins` of the release trust file
  (`testnet-trust.json`, schema `slop_ninja_testnet_trust_roots_v1`), which the
  release signature covers; or
- when no trust file pins an origin, the origin the miner names with
  `--coordinator`.

The public snapshot (section 10) is unsigned. Its `coordinator` field chooses a
destination only when the trust file pins that origin. When a trust file pins
origins, every other origin is refused, including one named with
`--coordinator`. The client prints the origin before it opens a session. It
never follows a redirect: a 3xx answer is an error, so signed headers reach only
that origin. Presigned PUTs go only to the R2 S3 host the URL names, after the
client's own check of the URL (section 6). `subnet/coordinator/testnet-trust.json` pins
`https://testnet-admission.slop.ninja`, the upload Worker's custom domain,
and the public snapshot advertises the same origin. A released client run
with `--public` therefore uploads there. Because the file pins an origin,
`--coordinator` with any other origin is refused.

## 5. Endpoints

All paths are under the coordinator's base URL. Responses are JSON
(`content-type: application/json`) with a `schema` field. Clients ignore
unknown fields; a change of meaning bumps the schema version.

A response body is at most 64 KiB (`response_limit` in the wire crate): the
largest response, a session with one fresh URL for the header and each file
of the spec's file table, is far smaller. Clients refuse longer bodies
without buffering them. A hotkey view that would exceed the limit lists the
live session and the newest ended sessions that fit.

### `GET /admission/v2/cohort` (unsigned)

The coordinator's admission parameters. Content-free. The response carries
`cache-control: no-store`, and the client asks for `no-cache`, because the
cursor in it dates request signatures.

```json
{
  "schema": "slop_ninja.admission.cohort/v2",
  "contract": "slop_ninja.admission/v2",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "finalized_block": 8340012,
  "request_validity_blocks": 360,
  "paused": false,
  "confirmed_epoch": 23169,
  "admitting": {
    "cohort": 7,
    "label": "testnet-c7",
    "opened_epoch": 23169,
    "open_block": 8339400,
    "planned_cutoff_epoch": 23170,
    "closing": false,
    "output_contract": "slop_ninja_full_a_contract_v2",
    "artifact_spec": { "id": "slop_ninja_a_artifact_spec_v2", "sha256": "<spec digest>" },
    "roster_cap": 40,
    "selection": "earliest_commitment/v1",
    "max_object_len": 954576228,
    "upload_policy": { "id": "slop_ninja_upload_url_policy_v1", "b_min": 125000, "skew_secs": 900 },
    "limits": "<hex digest of the cohort's limits record>"
  },
  "next": { "cohort": 8, "earliest_open_epoch": 23189, "admission_tempos": 16 }
}
```

`admitting` is `null` when no cohort is admitting. `closing` is true once the
coordinator has begun closing admission; new sessions are then refused.
`request_validity_blocks` is top-level because signed reads are also made
while no cohort is admitting. `upload_policy` sets each presigned URL's life
(section 6).

`next` names the next cohort: a queued one, else the one the standing template
opens next. `earliest_open_epoch` is a lower bound: a cohort opens only at the
boundary that closes the one before it, which is at least `K` tempos after
that one opened and two tempos after its publication; slow work or a
coordinator outage moves it later. `admission_tempos`, when present, is how many
tempos its admission window lasts once it opens (its plan's value, frozen with
the cohort); it is absent when the coordinator does not know the next plan.
With the plan known, `next` also carries its `output_contract`,
`max_object_len` and `artifact_spec` (the same meanings as in `admitting`), so
`check` can compare an object with them between windows; the plan is the
queued cohort's, else the standing template's, which can still change before
the cohort is queued.
`confirmed_epoch`, when present, is the subnet epoch index
(`SubnetEpochIndex`) the coordinator last observed at a finalized block: the
epoch the window's epochs are measured from. It is absent before the
coordinator has observed one. `slop_ninja_miner next-cohort` and `status`
show this read when no public status host is given. The example's epochs are
illustrative: `SubnetEpochIndex` is a per-subnet counter (about 113 for
subnet 601 in early November 2026), not the block number divided by 360.

`output_contract` is the artifact spec's contract. `artifact_spec` names the
spec whose checks admission runs, by id and by the digest
`slop_ninja_artifact_check spec` prints; a client built for another spec
reports its check as incomplete. The scorer admits each load by the spec
the load names by digest, and by spec v2 when it names none. A cohort's
scoring loads and its admission runs name its spec's digest, frozen in its
plan. Spec v3 admits every artifact spec v2 admits with the same results,
and adds the span tower class, which only a cohort naming spec v3 admits. `max_object_len` is derived from the cohort spec's
bounds: the 4-byte length prefix, the protocol's header bound
(`MAX_ARTIFACT_HEADER_LEN`), the largest graph class length the spec serves
(a class fixes its file length), the pinned tokenizer length and the spec's
budget for the JSON role files together (spec v2 and v3). It is recorded in
the cohort's `limits` record and changes only with the spec or a new limits
version; no deployment value enters it. Every object fits one R2 PUT (the
single-part limit is 5 GiB less 5 MiB), so there is no part size and no
multipart upload.

### `POST /admission/v2/sessions` (signed)

Opens, or returns, the upload session for one envelope. Body: exactly one
canonical `SubmissionUpload` record (version 2), media type
`application/vnd.slop-ninja.submission-upload.v2`: the opening, the
commitment, `header_sha256` (the SHA-256 of the header bytes `H`) and
`header_len` (`1..=MAX_ARTIFACT_HEADER_LEN`, 1 MiB). The session id is the
record's digest, so a lost answer loses nothing: the same envelope again
returns the same session.

The cohort authority checks, in order: the chain, that a cohort admits
(`not_admitting`, `after_cutoff`), the cohort (`wrong_cohort`), the signer is
the opening's hotkey (`hotkey_mismatch`), the header length
(`header_too_large`), the logged commitment (section 2), and the one-artifact
rule (`second_artifact`; a live session of an older commitment of the same
hotkey is `superseded`). It answers 201 with the session and `uploads` holding
one URL, the header's.

```json
{
  "schema": "slop_ninja.admission.session/v2",
  "session": "<hex>",
  "cohort": 7,
  "hotkey": "5F…",
  "commitment": "<hex>",
  "header_sha256": "<hex>",
  "header_len": 1022,
  "objects": [ { "kind": "header", "sha256": "<hex>", "len": 1022, "present": false } ],
  "state": "uploading",
  "refusal": null,
  "position": { "block": 8339500, "extrinsic": 3 },
  "next_cohort": null,
  "finalized_block": 8340012,
  "uploads": [ {
    "kind": "header", "sha256": "<hex>", "len": 1022,
    "url": "https://<account>.r2.cloudflarestorage.com/slop-ninja-admission-testnet/admission/v2/c7/<commitment>/h?X-Amz-…",
    "content_length": "1022", "if_none_match": "*", "checksum_sha256": "<base64 of the digest>",
    "signed_at": 1793846400, "valid_secs": 909
  } ],
  "repair_needed": []
}
```

### `GET /admission/v2/sessions/{session}` (signed)

The session, as above, with no `uploads`. `objects` lists the header and,
once the header is recorded, every file of its table with whether R2 holds
it as committed.

### `POST /admission/v2/sessions/{session}/urls` (signed, empty body)

Fresh presigned URLs for every object not yet recorded present. Every call
mints new ones, with no count or time limit; only protocol state refuses.

1. If the header is not yet recorded, the coordinator settles it first: R2's
   `head()` of the header key must show `size == header_len` and the stored
   SHA-256 equal to `header_sha256` (R2 verified it at upload), with no
   customer encryption key, the standard storage class and no stored HTTP or
   custom metadata. It reads the header bytes (at most 1 MiB, pinned to the
   entity tag it just saw) and asks the upload Worker to decode and check them
   against the spec's file table; it then recomputes the content digest from
   the returned table, requires it to be the committed one and the contract
   to be the cohort's, and runs `SubmissionUpload::admit_header` against the
   logged commitment.
   - No header object yet: `header_missing` (409, retryable), with a fresh
     header URL in the error's session.
   - A header that fails: the session ends `refused` with the code (section 8).
2. Otherwise: 200 with the session and one URL per file not yet recorded
   present (at most the spec's six files). An artifact doomed by its header is
   refused before the miner sends any file.

After the close begins, `urls` answers `after_cutoff`.

### `PUT <presigned URL>` (to R2, not to the coordinator)

The object's exact bytes, with exactly these headers (the HTTP stack adds
`host`), and nothing else `x-amz-*`, no `content-encoding`, never chunked
transfer encoding:

| Header | Value |
| --- | --- |
| `content-length` | the committed length |
| `if-none-match` | `*` |
| `x-amz-checksum-sha256` | the committed SHA-256, base64 |

All four (with `host`) are signed into the URL; leaving one out or changing it
fails the signature (403 `SignatureDoesNotMatch`). R2 hashes the body as it
arrives and refuses a mismatch (400 `BadDigest`), storing nothing. Section 6
gives the client's checks and retries.

### `POST /admission/v2/sessions/{session}/complete` (signed, empty body)

The coordinator heads every object it has not yet recorded and records those
R2 holds as committed (size, stored SHA-256 and the attributes above), with
their entity tags. An object present with other bytes or attributes is
deleted, and the miner may PUT it again.

- All present: 202, state `verifying`; the scorer's admission load is queued
  (section 7). The files manifest the scorer loads names each object's key
  and entity tag.
- Otherwise: 409 `objects_missing`, `detail.missing` listing the missing
  digests. The client asks for URLs again.

### `POST /admission/v2/sessions/{session}/repair` (signed, empty body)

For a recorded object that went missing after admission recorded it (a
platform fault, never the miner's): the coordinator heads every recorded
object first, then answers a fresh URL for exactly each missing recorded key,
digest and length, so R2 again fixes the content. An object found with its
committed bytes but a stored attribute (an unsigned `Content-Type`) is
deleted and listed missing too, so it can be PUT again cleanly. Valid in any
phase while the objects are still needed: until the cohort is closed and the
entrant's publication is recorded (an admitted entrant's scoring lanes read
them until the close); after that it answers `not_needed`. It needs no
current registration. The session's `repair_needed` lists such objects; a
scoring lane or publication that meets one absent starts the same check, and
waits, uncounted, until the object is back; it is never skipped.
`slop_ninja_miner repair` (and `status --repair`) re-PUTs them from the local
object, and asks for the check even when nothing is listed yet.

### `GET /admission/v2/cohorts/{cohort}/hotkeys/{hotkey}` (signed by that hotkey)

What the coordinator sees for one hotkey in one cohort, including when no
session exists. `slop_ninja_miner status` prints it, so every admission refusal
is visible to the miner: session refusals in `sessions`, and refusals of
session requests in `last_refusal`.

```json
{
  "schema": "slop_ninja.admission.hotkey/v2",
  "cohort": 7,
  "hotkey": "5F…",
  "finalized_block": 8340020,
  "registration": { "uid": 12, "registration_block": 8330000 },
  "commitment": {
    "block": 8339500,
    "block_hash": "<hex>",
    "extrinsic": 3,
    "payload": "<hex, 44 bytes>",
    "payload_cohort": 7,
    "in_window": true
  },
  "cutoff_block": null,
  "sessions": [ { "session": "<hex>", "state": "verified", "refusal": null, "repair_needed": [] } ],
  "last_refusal": {
    "code": "second_artifact",
    "message": "…",
    "detail": { "session": "<hex>" },
    "finalized_block": 8340019,
    "session": "<hex of the refused envelope's digest>",
    "opening_cohort": 7
  }
}
```

`registration` is the hotkey's registration as its commitment that counts
recorded it (at its commitment block), and `null` when it had no UID there or
has no logged commitment.
`commitment` is the hotkey's commitment that counts for the cohort, from the
commitment log: its latest one while the cohort admits, and its last one before
the cutoff block once admission has closed (`cutoff_block` is then set); it is
`null` when there is none. `last_refusal` is the latest refused session
request by the hotkey while this cohort admitted (or, once no cohort
admits, naming this cohort); the coordinator keeps one per hotkey and cohort,
content-free, for as long as it keeps the cohort's session records.

## 6. Direct upload

Each object goes to R2 in one presigned `PutObject`: the header bytes `H`
under `admission/v2/c<cohort>/<commitment>/h`, and each distinct file of the
header under `admission/v2/c<cohort>/<commitment>/f/<file sha256>`. The
signer derives every key from protocol values and never accepts one. The
largest file (the tower graph, 681,508,964 B) and the largest artifact
(954,576,228 B) are far below R2's single-PUT limit (5,363,466,240 B), so
there is no multipart upload and no part size.

**Why one object per file.** The commitment binds the header, and the header
binds each file's SHA-256. With one PUT per file, R2 verifies exactly the
digests the commitment rests on; a whole-object PUT would leave the files
inside unchecked without a second hash on our side.

**The client's checks before it sends a byte** (`slop_ninja_r2_presign::check`):

- the URL is `https`;
- its host is the release trust file's pinned upload host when one is pinned,
  else an R2 S3 host (`*.r2.cloudflarestorage.com`);
- its path is the bucket and exactly the key the client derives itself from
  (cohort, commitment, digest);
- its query has exactly the seven SigV4 parameters, `X-Amz-SignedHeaders`
  exactly `content-length;host;if-none-match;x-amz-checksum-sha256`,
  `X-Amz-Content-Sha256=UNSIGNED-PAYLOAD`, and `X-Amz-Expires` within R2's
  7 days.

It refuses a redirect.

**URL life (upload URL policy `slop_ninja_upload_url_policy_v1`, frozen in
each cohort plan).** `valid_secs = ceil(L_total / B_min) + S`, with `L_total`
the artifact's committed size (header plus every file, so a client that takes
every URL at once and starts them in sequence at `B_min` is covered),
`B_min` = 125,000 B/s (1 Mbit/s) and `S` = 900 s, at most R2's 604,800 s. For
the largest spec v3 artifact that is 8,537 s. R2 checks a URL's expiry when
the PUT starts, not when its body ends (qualified 2026-11-05: a body that
ended about 30 s after expiry was stored), so a PUT that started in time
completes at any rate: no upload rate is required, and `B_min` only sets how
long an unused URL waits.

**No expiring lease.** Expiry ends one URL, never the session or the
commitment: a miner whose commitment is valid gets fresh URLs on every
request. During a coordinator outage no new URL is minted, but issued URLs
still work, and the cutoff cannot pass while the coordinator is down.

**Retries** (per file; a PUT cannot resume):

- a transport error, a 5xx, 429 (R2 takes one write per second per key), or
  403 `ExpiredRequest`: ask `urls` for a fresh URL and send that file again
  from byte 0 (doubling backoff, except after expiry);
- 412 `PreconditionFailed`: the object exists (for example after a lost
  success answer); go on to `complete`, which confirms it;
- 400 `BadDigest`: the local object no longer matches its own header; stop
  and re-pack from the same sources, since the commitment binds the digests;
- 403 `SignatureDoesNotMatch`: a client defect; stop;
- 501 `NotImplemented`: R2's refusal, before authentication, of a URL
  carrying a parameter a presigned PUT does not take (`partNumber` with
  `uploadId`, `versionId`); nothing is stored. The URL checks above refuse
  such a URL before any byte, so this is a refusal like 403: stop.

Send only the URL's signed headers (and `host`). R2 stores six unsigned
standard headers a presigned PUT carries (`Content-Type`, `Content-Encoding`,
`Content-Disposition`, `Content-Language`, `Cache-Control`, `Expires`) as the
object's HTTP metadata, and admission refuses any object with stored HTTP or
custom metadata (rule `slop_ninja_object_attributes_v1`, named in the cohort's
limits record): an object with any, header or file, is deleted (only while it
still carries them) and listed missing again, so it is PUT once more under a
fresh URL; the commitment stays valid.

A restarted `upload` sends only the objects the session lists as missing. The
client streams each file from disk with one buffer, so its memory does not
grow with the artifact. `--min-upload-kib-per-sec` bounds each PUT's local
timeout only; nothing about the rate is sent.

## 7. The cutoff and the roster

When the coordinator closes admission at the cutoff block, it:

1. persists `closing` (from here `urls` answers `after_cutoff` and only the
   close's own sweep records presence), checks the upload path end to end
   with a canary PUT of its own bytes (a failure defers the close), heads every
   unrecorded object of every open session once (in batches within the
   platform's subrequest limit, the progress persisted), records the close,
   and refuses every session still missing an object (`incomplete_at_cutoff`).
   A head that errors defers the close; only a definite absence counts as
   missing. A signer or upload-path fault recorded since the last success also
   defers it, so the platform's outage never ends a miner's upload;
2. once every completed upload is verified or refused, every verdict is in and
   the snapshot at `cutoff_block - 1` is read (section 2), checks each
   `verified` session against the snapshot: the hotkey's commitment then must
   be the one the session opens, at its logged position, and its UID and
   registration block must be the logged ones. A session whose commitment a
   later one replaced is refused with `commitment_overwritten`; a changed
   registration gives `not_registered` or `registration_mismatch`. This
   happens at the cutoff's own tempo boundary when the work is in by then,
   else at the first boundary after it is;
3. refuses sessions whose commitment shares its position with another
   hotkey's (one extrinsic, such as a batch call, that committed for several
   hotkeys: `commitment_mismatch`, `detail.reason` `shared_extrinsic`), sorts
   the remaining sessions by position `(block, extrinsic)` and admits the
   first `roster_cap` of them (`admitted`), in that order, as the cohort
   manifest's roster (the `RosterEntry` values `SubmissionUpload::admit_header`
   returned);
4. marks the rest `overflow`, with `next_cohort` set.

`selection` names the rule; `earliest_commitment/v1` is the rule above, and any
other rule (for example a per-coldkey limit or a draw among the commitments
finalized before the cutoff) gets a new name.

**Overflow does not carry over.** An overflow session's opening names this
cohort, so it cannot open a commitment for the next one. To enter cohort `N+1`
the miner makes a fresh commitment (a new nonce and cohort `N+1`) inside
`N+1`'s window and uploads again. It is then ranked by that new commitment's
position. The overflow upload is deleted at the freeze and never published.

**Ordering and the official client.** `slop_ninja_miner commit` waits until the
coordinator reports the cohort as admitting, which lags the opening block by
the coordinator's observation delay. A script that commits at the first block
of the announced opening epoch counts too (when the cohort does open then), so
under `earliest_commitment/v1` and a roster cap scripted miners can take the
earliest positions. That belongs in the roster-selection decision; a random
draw among the commitments finalized within an initial grace window would
remove it, under a new `selection` name.

## 8. Errors

Every error response has this shape:

```json
{
  "schema": "slop_ninja.admission.error/v2",
  "error": { "code": "wrong_cohort", "message": "…", "retryable": false, "detail": { "admitting": 8 } }
}
```

Retryable errors carry `Retry-After` in seconds when the coordinator knows a
useful wait. Clients retry them with backoff, waiting as `Retry-After` says,
and never retry the others unchanged.

| Code | HTTP | Retryable | Meaning |
| --- | --- | --- | --- |
| `bad_request` | 400 | no | malformed path, headers or body |
| `unsupported_media_type` | 415 | no | wrong body type |
| `bad_signature` | 401 | no | the signature does not verify for `x-sn-hotkey` |
| `request_expired` | 401 | yes, re-sign | `valid_until` is below the finalized cursor (`detail.finalized_block`) |
| `request_validity_too_long` | 400 | no | `valid_until` exceeds the allowed window |
| `content_digest_mismatch` | 400 | no | the body does not match `x-sn-content-sha256` |
| `forbidden` | 403 | no | the signer does not own the session or path |
| `not_found` | 404 | no | unknown session or cohort |
| `not_admitting` | 409 | yes, later | no cohort is admitting (`detail.next`) |
| `after_cutoff` | 409 | no | admission has closed for this cohort |
| `wrong_chain` | 422 | no | the opening names another chain or subnet |
| `wrong_cohort` | 422 | no | the opening names another cohort (`detail.admitting`) |
| `hotkey_mismatch` | 422 | no | the opening names another hotkey than the signer |
| `not_registered` | 422 | no | the hotkey has no UID on the subnet (the upload Worker refuses every signed request from such a hotkey before reading its body) |
| `registration_mismatch` | 422 | no | the opening names another registration generation |
| `commitment_not_found` | 409 | yes | no logged commitment yet (none, or not yet final) |
| `commitment_unresolvable` | 422 | no | the hotkey's latest commitment can never be logged (`detail.reason`: `shared_extrinsic`, `undecodable`, `state_discarded`, `block_unknown`, `no_commitment_event`, `state_unavailable`); commit again with a new nonce |
| `commitment_before_open` | 422 | no | the commitment was made before the cohort opened |
| `commitment_mismatch` | 422 | no | the opening does not open the hotkey's current commitment (`detail.payload_cohort` when parseable), or (at the cutoff) the commitment shares its extrinsic with another hotkey's (`detail.reason` `shared_extrinsic`) |
| `header_too_large` | 413 | no | `header_len` is zero or exceeds `MAX_ARTIFACT_HEADER_LEN` |
| `second_artifact` | 409 | no | a session already exists for this commitment, or another live session for this hotkey and commitment (`detail.session`) |
| `session_not_uploading` | 409 | no | the session has ended and takes no URL request or completion |
| `header_missing` | 409 | yes, PUT it | `urls` before R2 holds the header; the answer carries a fresh header URL |
| `objects_missing` | 409 | yes, PUT them | completion before every object is in R2 as committed (`detail.missing`: digests) |
| `not_needed` | 409 | no | `repair` for an object that is present or no longer needed |
| `rate_limited` | 429 | yes | reserved; testnet enforces no request rate (section 13, "Limits") |
| `paused` | 503 | yes | the coordinator is paused or catching up; the cutoff extends |
| `unavailable` | 503 | yes | temporary failure |
| `internal` | 500 | yes | unexpected failure |

Session refusals (in `session.refusal`):

| Code | When | Meaning |
| --- | --- | --- |
| `object_mismatch` | header | R2's stored size or checksum of the header is not the envelope's (a header with only a forbidden stored attribute, rule `slop_ninja_object_attributes_v1`, is deleted and PUT again instead) |
| `artifact_invalid` | header | the header bytes do not decode or validate (`detail.reason`) |
| `contract_mismatch` | header | the header's contract is not the cohort's output contract |
| `content_mismatch` | header | the header's derived content is not the committed content |
| `check_failed` | header, admission run | an artifact check refused it (`detail.check`) |
| `object_replaced` | verify | before admission, a recorded object is present with other bytes or another entity tag: the miner's, never a cohort pause (one with its committed bytes and only a forbidden stored attribute is deleted and listed for repair instead) |
| `commitment_overwritten` | cutoff | a later commitment before the cutoff block replaced the one this session opens |
| `incomplete_at_cutoff` | cutoff | the session was not completed before the close |
| `not_registered`, `registration_mismatch` | cutoff | the registration changed before the cutoff |

Session states: `uploading`, `verifying`, `verified`, `refused`, `admitted`,
`overflow`, `superseded`.

The admission refusals of the announce-readiness plan map onto these codes. A
session refusal appears in the hotkey view's `sessions`; a refused session
request appears in its `last_refusal` (recorded only while the cohort
admits, and only for a hotkey the commitment reader has seen commit on the
subnet).

| Case | Code |
| --- | --- |
| no commitment, or not yet finalized | `commitment_not_found` |
| a commitment that cannot be resolved | `commitment_unresolvable` |
| wrong cohort, chain or hotkey | `wrong_cohort`, `commitment_mismatch`, `wrong_chain`, `hotkey_mismatch`, `registration_mismatch` |
| overwritten commitment | `commitment_mismatch` (at the session) or `commitment_overwritten` (at the cutoff) |
| a second artifact for the same hotkey | `second_artifact` |
| upload after the cutoff | `after_cutoff`, `incomplete_at_cutoff` |
| header larger than the bound | `header_too_large` |
| graph not allow-listed | `check_failed`: `graph_not_allowed`, `graph_op_not_allowed`, `graph_class` |
| more parameters than the graph class's bound (160M; under spec v3, 173,549,185 for the tower class) | `check_failed`: `too_many_parameters` |
| tokenizer not byte-identical | `check_failed`: `tokenizer_mismatch` |
| role-schema violation | `check_failed`: `schema`, `binding`, `file_role` |
| custom-op domain or external data | `check_failed`: `graph_custom_domain`, `graph_external_data` |
| undeclared files | `check_failed`: `undeclared_file` |

## 9. Privacy and retention

- Objects and openings stay in the private admission bucket until scoring
  starts and are never readable through this API. When a cohort's scoring
  starts (roster frozen), the publisher copies each admitted entrant's files
  and then its header into the public bucket (`blobs/sha256/<file sha256>` and
  `artifacts/v2/<header sha256>/header`), hashing nothing. The default copy
  (`ARTIFACT_COPY = "stream"`, until probe G7-x passes and the copy token is
  installed) pipes the admission object into a write that carries its
  SHA-256, which R2 verifies, and is checked by that stored SHA-256. The
  server-side copy (`ARTIFACT_COPY = "server"`, configuration, its own
  deploy) is an S3 `CopyObject` inside R2, presigned by
  the internal signer with its own copy credential, with the source entity
  tag pinned, `x-amz-metadata-directive: REPLACE` (no stored header of the
  source carries over), a fixed content type and the standard storage class.
  `CopyObject` keeps the entity tag but not the stored SHA-256, so the public
  copy is checked by its size and by entity-tag equality with the admitted
  object, whose SHA-256 R2 verified at upload. Operator and synthetic hotkeys and reference digests are withheld. The
  openings are published in the signed `CohortClosure`.
- Refused, overflow and superseded uploads are deleted once the roster
  freezes and are never published. Admitted objects are deleted from the
  admission bucket once the cohort closes and their publication is recorded
  (the publisher reports each entrant published or withheld to the eval
  Worker, and an absent object as missing, which starts its repair).
- A standing reconciliation lists `admission/v2/` and deletes any object no
  open session, verifying session or admitted entrant still needing it holds
  (a stale URL can recreate one); a delete queued after a failed one never
  removes a key such a session holds. It never ends on time or on an empty list, and gates nothing.
  There is no time-based R2 lifecycle rule on the bucket.
- Session records and last refusals are content-free (digests, lengths,
  states, codes, positions, URL mint counts) and stay readable at least until
  the cohort closes; after that the published package is the record.
- Uploads are protected in transit by TLS to the R2 S3 host. The miner sees
  the account's S3 host, the bucket name and the signing key's id in each URL;
  none of these is a credential. There is no separate encryption layer: R2 can
  only verify the digest of the bytes it receives.
- On the miner's machine, the object and the openings are files of mode 0600.

## 10. Public reads the client depends on (planned public layout)

With `--public`, `status` and `next-cohort` read the public status snapshot,
unauthenticated; otherwise they read the coordinator's unsigned cohort read
(`GET /admission/v2/cohort`).
The public publisher owns the layout, which is planned and not yet published;
the client reads only these fields and ignores the rest:

`GET {public}/v1/status.json`

```json
{
  "schema": "slop_ninja.public.status/v1",
  "network": "testnet",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "confirmed": { "block": 8340012, "epoch": 23170, "age_secs": 40 },
  "phase": "admission",
  "mode": "burn",
  "fault": false,
  "notice": "…",
  "admitting": { "cohort": 7, "label": "testnet-c7", "opened_epoch": 23169, "planned_cutoff_epoch": 23170 },
  "next": { "cohort": 8, "earliest_open_epoch": 23189, "admission_tempos": 16 },
  "coordinator": "https://…",
  "active_target": "<hex or null>",
  "reference": "<hex or null>"
}
```

The snapshot is unsigned, and the client labels it so. It is information only:
its `coordinator` field becomes an upload destination only when the trust file
pins that origin (section 4). The coordinator's `GET /admission/v2/cohort` is
authoritative for uploads; the snapshot is a published mirror and can lag it.

## 11. Not in v2

- S3 API credentials, or any credential other than the hotkey and the
  presigned URLs the coordinator mints for the session's owner.
- Multipart uploads or resumable transfers within one file.
- Carrying a commitment, a queue place or an uploaded object to a later cohort.
- Per-coldkey limits or a random or stake-weighted draw under the roster cap
  (a user decision; it would get a new `selection` name).
- Encryption of the upload to the coordinator key.
- A coordinator signature over the cohort response and the status snapshot; v2
  authenticates the destination by its pinned origin and TLS.

## 12. Notes for the coordinator implementation

- Serialize session operations, R2 object observations, the sweeps and the
  close in one Durable Object per cohort. Once `closing` is persisted, only
  the close sweep records presence, and its progress is persisted per key so
  it resumes after an eviction.
- Record presence only from R2's `head()`: size, the stored SHA-256, the entity
  tag, and no customer key, non-standard storage class or stored metadata.
  An object-create event on the bucket's `admission/v2/` prefix is a wakeup
  only (its payload carries no checksum); the client's `urls` and `complete`
  requests and the alarm's sweep settle objects too, so a lost event loses
  nothing.
- Keep one constant-size aggregate per session for minted URLs (a count and
  the latest expiry), never a row per mint. One `urls` call is one signer
  request (at most one header and the spec's file table).
- A mint failure that is the platform's (the signer unreachable, a 5xx) is a
  cohort fault (`signer_unavailable`), a failed canary is
  `upload_path_unavailable`, and a header settle or link probe the platform
  could not run (R2, the upload Worker's header route, or its registration
  read) is `upload_front_unavailable`; each defers the close until a later
  success, and the close begins with a fresh canary that also runs the link
  probe. URLs minted after the close was recorded are never handed out
  (`after_cutoff`).
- Keep every bound (`max_object_len`, `request_validity_blocks`, the URL
  policy) as a versioned parameter, and keep responses within
  `response_limit`.
- Keep the commitment log of section 2 on the finalized cursor. Session
  opening and the close read only the log.
- Record one `last_refusal` per hotkey and cohort, overwritten by the next
  refusal, after the signature verifies.
- Answer `GET /admission/v2/cohort` with `cache-control: no-store`, and put the
  cursor in `request_expired`'s `detail.finalized_block`. (On testnet the
  cohort authority writes the response, with the time, to a snapshot at every
  heartbeat, and the upload Worker serves the read from one edge-cache copy
  per location kept for one block, 12 s, refreshed on a miss through the eval
  Worker's internal snapshot operation, which answers from the snapshot
  object without reaching the cohort object. A snapshot older than the
  heartbeat freshness, 10 minutes, answers `paused: true`. This unsigned read
  never reaches the cohort authority.)

## 13. Implementation notes (coordinator, testnet)

- **Where.** The public routes exist only on the upload Worker
  (`slop-ninja-admission-testnet`, `subnet/admission-worker`), whose only
  public ingress is the custom domain `testnet-admission.slop.ninja`. It has
  no `workers.dev` name and no preview URLs. `ADMISSION_ROUTES` there is
  `open` (every registered hotkey), `hotkeys` (only those `ADMISSION_HOTKEYS`
  lists, for operator drills) or `off` (404). The eval Worker answers 404 to
  every `/admission/` request. Its internal interface
  (`/internal/admission/v2/<op>`, `slop_ninja_admission_wire::internal`)
  serves only requests addressed to its internal host that also present the
  interface token the two Workers share. The cohort authority authenticates
  each forwarded request again against the cursor and re-derives the route
  fields from the signed method and path, and requires the signer to be the
  session's hotkey.
- **Keys and bindings.** The upload Worker holds no key and no R2 binding;
  its one secret is the interface token, and its one binding is the service
  binding to the eval Worker (every forwarded request and the snapshot
  refresh, `/internal/admission/v2/snapshot`). The signer Worker holds the R2 S3 credential (Object
  Read & Write on the admission bucket only, from Secrets Store) and the S3
  host; it signs PUT only, for keys it derives, and never touches an object.
  The eval Worker binds the admission bucket (heads, header reads, deletes)
  and writes the cohort snapshot to the eval bucket, which neither R2 S3
  credential reaches, and has service bindings to the signer and to the
  upload Worker's header route (`POST /internal/upload/v2/header`, the same
  internal host and token). No upload byte reaches any of them.
- **Limits.** Testnet enforces protocol bounds only (decision D6): a request
  must be signed by a hotkey registered on subnet 601; one session per hotkey
  and commitment; one artifact per hotkey per cohort; each URL bound to its
  derived key, the committed length and digest, and create-only. There is no
  per-IP or per-hotkey rate cap, and `rate_limited` is never sent. A URL
  holder can make R2 refuse garbage bodies repeatedly; each attempt is a write
  operation on that miner's own key (at most about one per second), and the
  session's mint count names the hotkey.
- **Parameters.** `request_validity_blocks`, `roster_cap`, `selection`, the
  artifact spec (id and digest), the upload URL policy, the operator's own
  hotkeys and the fresh pool are the plan's `admission` block. Every
  admission run names the cohort's spec digest on its load.
- **Outside entrants**, **commitment log** and **snapshot**: as in version 1;
  section 2 and the coordinator README describe them.
- **Hash once.** The scorer Container receives each file in 64 MiB chunks at
  their offsets, read from R2 by ranged reads pinned to the recorded entity
  tag, and loads them with no hash; the spec's role binding and load guards
  compare the header's digests. The one remaining hash over model bytes is
  the graph allow-list's structure digest (decision D5, open). A source test
  (`no_object_hash`) fails if any other hash reaches the admission, scoring or
  check paths.

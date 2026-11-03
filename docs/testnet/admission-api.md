# Admission API (testnet, contract `slop_ninja.admission/v1`)

This is the contract between the coordinator's admission routes and the miner
client `slop_ninja_miner` (`subnet/crates/miner`). It covers the on-chain
commitment and how the coordinator reads it, the hotkey-signed upload into the
coordinator's private evaluation bucket, the admission cutoff, the
one-artifact-per-hotkey rule, the status endpoints and the error codes. It
applies to testnet subnet 601.

It is built on the protocol crate's submission types in
`subnet/crates/protocol/src/submission.rs` and the artifact object in
`subnet/crates/protocol/src/artifact.rs`:
`SubmissionOpening`, `SubmissionUpload`, `FinalizedCommitment`, `RosterEntry`,
`ArtifactReader` and `VerifiedArtifact`. Nothing here changes those types. The
artifact checks are those of the *A* artifact spec
(`docs/testnet/artifact-spec.md`, crate `slop_ninja_artifact_check`).

The byte-level helpers below (request message, route parser, session id, part
plan, error codes, response limit) are implemented once in the crate
`slop-ninja-admission-wire` (`subnet/crates/miner/wire`). It performs no I/O and
keeps no global state. Its arena-safe items (the request message, the borrowed
route parser `Route::parse`, the `path::write_*` writers, `PartPlan`, the codes,
the header parsers and `response_limit`) allocate nothing and build for
`wasm32-unknown-unknown`, so Workers code can use them inside a bounded memory
arena. Its owned JSON shapes (feature `json`) allocate; they serve the native
client and the test mock.

Status: the miner client implements this contract and is tested against a local
mock of it. The coordinator implements it on the eval Worker
(`subnet/coordinator/src/eval/admission.rs`, `commitments.rs`, `calendar.rs`
and `src/cf_admission.rs`), tested natively and for wasm32; it is not yet
deployed or drilled live. Every value under "Parameters" is announced by the
coordinator rather than assumed by the client: each is a parameter of the
cohort's frozen plan (its `admission` block) or derived from the plan's
artifact spec. Section 13 lists where the implementation refines this text.

## 1. Flow

1. `next-cohort` reads the public status snapshot (section 10) and reports the
   admitting cohort, or the next one and its earliest opening epoch.
2. `pack --dir MODEL_DIR` builds the artifact object `u32_le(len(H)) || H ||
   files` (`artifact.rs`) from the artifact spec's six files, with their fixed
   roles and the spec's output contract. `check` verifies the object and runs
   the spec's admission checks, the same streaming checks the coordinator runs.
3. `commit` makes a fresh secret 256-bit nonce and the opening for the
   admitting cohort, stores the opening in a local file with mode 0600, and
   signs `Commitments.set_commitment` with the hotkey (section 2).
4. After the commitment finalizes and the coordinator has logged it,
   `upload` opens a session at an authenticated coordinator origin (section
   4), uploads the object in parts through the coordinator's Worker, and
   completes the session (sections 4 to 6). An interrupted upload resumes from
   the parts the coordinator has recorded.
5. The coordinator verifies the object and the envelope at completion and
   again at the cutoff, ranks the remaining entrants and freezes the roster
   (section 7). `status` shows the result, including every refusal (section
   5, the hotkey view).

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
  the hotkey's registration at `c`. These reads come within minutes of the
  commitment, inside the state window ordinary nodes keep. If the node no
  longer serves `c`'s state, the commitment stays unresolved: the
  coordinator never substitutes a later state for an earlier block.
- **Snapshot.** When admission closes, for every hotkey whose upload
  completed in time, it reads `CommitmentOf`, `Uids` and then
  `BlockAtRegistration` at the hash of `cutoff_block - 1`: the state the
  last block before the cutoff left. That is exactly the hotkey's last
  commitment before the cutoff block and the registration that held then.
  The roster does not freeze before the snapshot is read.
- Opening a session uses the hotkey's latest logged commitment; while a newer
  one the poll saw is still being resolved, opening answers
  `commitment_not_found` (retryable). The close uses the snapshot.
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
- **Uploads** count when the coordinator recorded the session's completion
  before it recorded the close. Session operations and the close are serialized
  by the same Durable Object, so every part and completion request is either
  recorded before the close or refused with `after_cutoff`. Verification that
  was accepted before the close finishes after it and still counts.

A commitment needs finality and the coordinator's logging of it (the nominal
wait is at most 60 seconds after finality, and it can be longer) before a
session can open. An object of about 610 MB must then be uploaded before the
cutoff. Commit and upload early in the window.

## 4. Signed requests

Every request except `GET /admission/v1/cohort` is signed by the hotkey that the
request concerns. There are no accounts, tokens, cookies or presigned URLs.

Headers:

| Header | Value |
| --- | --- |
| `x-sn-hotkey` | SS58 address (prefix 42) of the signing hotkey |
| `x-sn-valid-until` | decimal finalized block number after which the request is refused |
| `x-sn-content-sha256` | lowercase hex SHA-256 of the request body (of the empty string when there is none); for a part, the part's digest |
| `x-sn-signature` | lowercase hex of the 64-byte sr25519 signature |

The signed message is 32 bytes:

```
message = SHA-256( u32_le(len(D)) || D || body )
D       = "slop_ninja.admission/request/v1"
body    = genesis (32) || u16_le(netuid) || u8(mechanism)
        || hotkey public key (32)
        || u64_le(valid_until)
        || u32_le(len(method)) || method        e.g. "PUT"
        || u32_le(len(path))   || path          e.g. "/admission/v1/sessions/<id>/parts/3"
        || content sha256 (32)
```

This is the protocol's `domain_hash(D, body)` with the protocol codec's
integer and string conventions. The path is the request path exactly as sent,
without scheme, host or query; no route uses a query string. The hotkey signs
the 32-byte message itself with sr25519 under the Substrate signing context,
with no `<Bytes>` wrapper (rtao `Signer::sign_raw`). The coordinator verifies it
strictly (`rtao_core::keys::verify_strict`, as `verify_sr25519` in
`subnet/coordinator/src/chain.rs` does).

The coordinator refuses a request when:

- the signature does not verify (`bad_signature`);
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
- a part `PUT` carries the part's digest, so sending it again records the same
  bytes again;
- a completion completes only what was uploaded, and returns the session once
  it is complete;
- `POST /admission/v1/sessions` returns an existing session in whatever state
  it is and never reopens an aborted one;
- abort and reopen name the session's attempt (section 5). Every reopen starts
  a new attempt, so an abort or reopen sent again from an earlier attempt is
  refused with `stale_attempt`, and sending again the request that made the
  current state returns it unchanged.

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
never follows a redirect: a 3xx answer is an error, so signed headers and parts
reach only that origin. `subnet/coordinator/testnet-trust.json` pins no
admission origin yet: the origin and the Worker that serves it wait for the
public-access decision, and until a release pins one, clients name the
coordinator with `--coordinator`.

## 5. Endpoints

All paths are under the coordinator's base URL. Responses are JSON
(`content-type: application/json`) with a `schema` field. Clients ignore
unknown fields; a change of meaning bumps the schema version.

A response body is at most 64 KiB plus 256 bytes for each of the admitting
cohort's `max_parts` (`response_limit` in the wire crate): no response lists
more than `max_parts` part records or missing part numbers, and the largest
compact part record has 122 bytes. The cohort read and the public snapshot,
read before `max_parts` is known, are at most 64 KiB. Clients refuse longer
bodies without buffering them. A hotkey view that would exceed the limit lists
the live session and the newest ended sessions that fit.

### `GET /admission/v1/cohort` (unsigned)

The coordinator's admission parameters. Content-free. The response carries
`cache-control: no-store`, and the client asks for `no-cache`, because the
cursor in it dates request signatures.

```json
{
  "schema": "slop_ninja.admission.cohort/v1",
  "contract": "slop_ninja.admission/v1",
  "chain": { "genesis": "8f9c…3105", "netuid": 601, "mechanism": 0 },
  "finalized_block": 8340012,
  "request_validity_blocks": 360,
  "paused": false,
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
    "max_object_len": 700000000,
    "part_size": 67108864,
    "max_parts": 10000,
    "limits": "<hex digest of the cohort's limits record>"
  },
  "next": { "cohort": 8, "earliest_open_epoch": 23189 }
}
```

`admitting` is `null` when no cohort is admitting. `closing` is true once the
coordinator has begun closing admission; new sessions are then refused.
`request_validity_blocks` is top-level because signed reads and aborts are
also made while no cohort is admitting.

`output_contract` is the artifact spec's contract. `artifact_spec` names the
spec whose checks admission runs, by id and by the digest
`slop_ninja_artifact_check spec` prints; a client built for another spec
reports its check as incomplete. The scorer admits each load by the spec
the load names by digest, and by spec v2 when it names none. The
coordinator's loads name no spec yet, so they are admitted by spec v2, the
spec every testnet cohort names; a cohort that names spec v3 needs its loads
to name it. Spec v3 admits every artifact spec v2 admits with the same
results, and adds the span tower class, which only a cohort naming spec v3
admits. `max_object_len` is derived from the cohort spec's
bounds: the 4-byte length prefix, the protocol's header bound
(`MAX_ARTIFACT_HEADER_LEN`), the largest graph class length the spec serves
(a class fixes its file length), the pinned tokenizer length and the spec's
budget for the JSON role files together (spec v2 and v3). It is recorded in
the cohort's `limits` record and changes only with the spec or a new limits
version; no deployment value enters it. The value above is an example, not a
derivation.

### `POST /admission/v1/sessions` (signed)

Opens, or returns, the upload session for one envelope.

- Body: the canonical bytes of the protocol record `SubmissionUpload`
  (`Record::to_bytes()`), with
  `content-type: application/vnd.slop-ninja.submission-upload.v1`. The record
  has a fixed size; the coordinator refuses a larger body with `bad_request`.
- Session id: lowercase hex of `SubmissionUpload::digest()`. The client can
  compute it before the request, so a lost response never loses the session.
- Checks, in order: the record validates; `opening.chain` is the coordinator's
  chain (`wrong_chain`); a cohort is admitting (`not_admitting`) and
  `opening.cohort` is it (`wrong_cohort`); `opening.hotkey` is the signer
  (`hotkey_mismatch`); `artifact_len` is at most `max_object_len` and fits in
  `max_parts` parts (`object_too_large`); the hotkey held a UID at its
  commitment (`not_registered`) under the opening's generation
  (`registration_mismatch`);
  the commitment log holds a commitment of the hotkey (`commitment_not_found`),
  made inside the window (`commitment_before_open`), that this opening opens
  (`commitment_mismatch`); and the one-artifact rule below (`second_artifact`).
  While no cohort is admitting, an opening for a cohort whose admission has
  closed gets `after_cutoff`, and any other `not_admitting`.
- The same envelope returns the existing session (200) in whatever state it
  is, `aborted` and `refused` included, before any of the checks above: they
  ran when it was created, and a later commitment changes nothing about it.
  Reopening is its own request.
- One artifact per hotkey per cohort: a hotkey has at most one live session,
  and one session per commitment.
  - A different envelope that opens the same commitment is refused with
    `second_artifact` (409), with the existing session's id in
    `detail.session`, whatever that session's state: the miner reopens it
    (when aborted) or commits again to submit another artifact.
  - An envelope that opens a newer commitment (the hotkey committed again)
    supersedes the earlier session, whose state becomes `superseded` and whose
    parts are deleted.
- Every refusal of this request, and of a reopen, after the signature verifies
  is recorded as the hotkey's `last_refusal` for the cohort (the hotkey view),
  because no session record exists to carry it.
- Response: 201 with the session (below) when created, 200 when it existed.

### `GET /admission/v1/sessions/{session}` (signed)

The session, including the parts the coordinator has recorded. A client resumes
by uploading every part that is missing here.

```json
{
  "schema": "slop_ninja.admission.session/v1",
  "session": "<hex>",
  "cohort": 7,
  "hotkey": "5F…",
  "commitment": "<hex>",
  "artifact_sha256": "<hex>",
  "artifact_len": 610612345,
  "part_size": 67108864,
  "part_count": 10,
  "attempt": 1,
  "parts": [ { "part": 1, "len": 67108864, "sha256": "<hex>" } ],
  "state": "uploading",
  "refusal": null,
  "position": null,
  "next_cohort": null,
  "finalized_block": 8340020
}
```

`attempt` starts at 1 and grows by one each time an aborted session reopens; a
new attempt starts with no parts. `refusal`, when present, is
`{ "code", "message", "detail" }` with a code from section 8. `position` is
`{ "block", "extrinsic" }` once the coordinator has read the commitment.
`next_cohort` is set for `overflow`.

### `PUT /admission/v1/sessions/{session}/parts/{n}` (signed)

Uploads part `n` (1-based) as the raw request body.

- `Content-Length` is required and must be the part's exact length
  (section 6); `x-sn-content-sha256` is the part's SHA-256.
- The Worker streams the body into the R2 multipart upload through its bucket
  binding (`uploadPart`); it never buffers the part. Where the digest-stream
  path is qualified it also hashes the stream and refuses a mismatch with
  `part_digest_mismatch`, after which the part may be sent again. Where it is
  not, it records the claimed digest, and a part that changed in transit
  surfaces at completion as `object_mismatch`. The coordinator today records
  the claimed digest. Clients handle both: on `object_mismatch`,
  `upload --restart` aborts the attempt and sends every part again in a new
  one.
- Re-sending a recorded part replaces it; the latest one counts.
- Errors: `part_out_of_range`, `part_length_mismatch`, `part_digest_mismatch`,
  `session_not_uploading`, `after_cutoff`.
- Response 200: `{ "part", "len", "sha256" }` as recorded.

### `POST /admission/v1/sessions/{session}/complete` (signed, empty body)

Requires every part (`parts_missing`, with the missing part numbers in
`detail.missing`; the client uploads those and asks again). The coordinator
answers 202 with the session in state `verifying` and completes the R2
multipart upload as durable work: a completion whose outcome was not
recorded (an eviction, a transport error) is issued again, by the
coordinator itself and on a repeated `complete`, until R2 completes it or
the object is found at the envelope's length; only R2's definitive refusal
of the parts (a part missing or changed, or an upload gone with no object)
ends the session with `object_mismatch`. It then, as its own durable work:

1. streams the object through `ArtifactReader` (bounded memory: one fixed
   buffer; the header is at most 1 MiB and is decoded once, and a header
   length of zero is refused) and requires the object's SHA-256 and length
   to be the envelope's (`object_mismatch`) and the object to verify
   (`artifact_invalid`). A header listing more files than the spec's file
   table is refused at once (`check_failed`, `undeclared_file`). Framing
   that is cut off (CPU or wall limits, an eviction) as many times as the
   plan's `artifact_failures_before_model_error` refuses the session
   (`artifact_invalid`, `detail.reason` `framing_incomplete`); a failed read
   of the object is the coordinator's and never counts. Framing waits while
   the hotkey's newest commitment is still being logged;
2. runs the artifact spec's header-only file-table checks (contract, names,
   roles, tokenizer byte-match, declared lengths, graph class length) and
   cuts each file into the scorer's content-addressed parts and parts
   manifest (`check_failed`, with the spec's code in `detail.check`);
3. requires the header's contract to be the cohort's `output_contract`
   (`contract_mismatch`);
4. runs `SubmissionUpload::admit` against the hotkey's logged commitment
   (`content_mismatch` when the object's derived content is not the committed
   one, or the commitment codes above);
5. runs the artifact spec's whole check in an admission run on the scorer
   Container: a fresh, single-use instance loads the parts for the cohort,
   and the scorer's own admission (graph pre-parse included) runs before ONNX
   Runtime opens anything. A definitive refusal ends the session with
   `check_failed` and the scorer's code in `detail.check` (such as
   `undeclared_file`, `tokenizer_mismatch`, `graph_op_not_allowed` or
   `too_many_parameters`; `docs/testnet/artifact-spec.md` lists them). The
   miner's `check` reports the same codes. There is one admission run per
   hotkey and commitment: another object for the same commitment is refused
   with `detail.check` `admission_run_used` (commit again to submit another).
   The run also scores one fixed control document; the artifact is admitted
   only when it does. A load that fails, dies or outlasts its allowance on
   fresh instances, a host read fault the scorer files under `object`, or a
   control document that does not score, as many times as the plan's
   `artifact_failures_before_model_error`, gives `load_failed:<reason>`;
   platform failures are retried with a growing backoff and never count. A
   verdict records the scorer build and spec it was reached on; when either
   differs from the deployment's, it runs again before the roster freezes,
   refusals included, and the session shows `verifying` meanwhile.

No parser of the graph or the role files runs in the eval Worker, which holds
the package and receipt signing keys; steps 1 to 4 read only the header
record, lengths and digests. An operator can refuse a stalled session with a
recorded reason (`check_failed`, `detail.check` `operator:<reason>`).

The session ends in `verified` or `refused`. The client polls the session.

### `DELETE /admission/v1/sessions/{session}/attempts/{attempt}` (signed)

Aborts attempt `attempt` of the session before the cutoff: the multipart upload
is aborted, stored parts and objects are deleted, and the state becomes
`aborted`. It applies to a session that is `uploading`, `verified` or
`refused`, and only when `attempt` is the session's current attempt
(`stale_attempt` otherwise, with the current attempt in `detail.attempt`).
Aborting an attempt that is already aborted returns the session unchanged. The
hotkey can then open another session for its current commitment. A refused
session stays refused for its envelope until its attempt is aborted.

### `POST /admission/v1/sessions/{session}/attempts/{attempt}` (signed, empty body)

Reopens an aborted session, empty, as attempt `attempt`, which must be its
current attempt plus one; otherwise `stale_attempt`. The same checks as opening
a session apply (commitment, window, registration, the one-artifact rule).
Sending it again while that attempt is live returns the session unchanged.

### `GET /admission/v1/cohorts/{cohort}/hotkeys/{hotkey}` (signed by that hotkey)

What the coordinator sees for one hotkey in one cohort, including when no
session exists. `slop_ninja_miner status` prints it, so every admission refusal
is visible to the miner: session refusals in `sessions`, and refusals of
session requests in `last_refusal`.

```json
{
  "schema": "slop_ninja.admission.hotkey/v1",
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
  "sessions": [ { "session": "<hex>", "state": "verified", "attempt": 1, "refusal": null } ],
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
`null` when there is none. `last_refusal` is the latest refused session or
reopen request by the hotkey while this cohort admitted (or, once no cohort
admits, naming this cohort); the coordinator keeps one per hotkey and cohort,
content-free, for as long as it keeps the cohort's session records.

## 6. Parts

Uploads go through the Worker's R2 binding (`createMultipartUpload`,
`uploadPart`, `complete`, `abort`). There are no presigned URLs and no S3 API
credentials. Each part is one Worker request.

- `part_size` (`P`) comes from the cohort parameters. Part `n` covers bytes
  `[(n-1)·P, min(n·P, L))` of the object of length `L`; every part but the last
  has exactly `P` bytes. `part_count = ceil(L / P)`.
- `P` must be at least R2's multipart minimum of 5 MiB and fit within the
  Workers request-body limit of the account's plan. The coordinator chooses it
  so that `max_object_len` fits in `max_parts` parts (R2 allows at most 10,000).
  It is a versioned deployment parameter; the client never assumes a value.
- The client holds one part in memory at a time, shared across retries, so its
  memory use is about `P`. It bounds a part's request by its base timeout plus
  the part's size at a slowest acceptable upload rate the miner chooses
  (`--min-upload-kib-per-sec`), so a slow uplink is not cut off by a fixed
  timeout.
- Retryable errors are retried with doubling backoff; a `Retry-After` from the
  coordinator sets the wait instead.

## 7. The cutoff and the roster

When the coordinator closes admission at the cutoff block, it:

1. refuses every session not yet completed (`incomplete_at_cutoff`) and aborts
   its upload;
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
   manifest's roster (the `RosterEntry` values `SubmissionUpload::admit`
   returned);
4. marks the rest `overflow`, with `next_cohort` set.

`selection` names the rule; `earliest_commitment/v1` is the rule above, and any
other rule (for example a per-coldkey limit or a draw among the commitments
finalized before the cutoff) gets a new name.

**Overflow does not carry over.** An overflow session's opening names this
cohort, so it cannot open a commitment for the next one. To enter cohort `N+1`
the miner makes a fresh commitment (a new nonce and cohort `N+1`) inside
`N+1`'s window and uploads again. It is then ranked by that new commitment's
position. The overflow upload is deleted at the close and never published.

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
  "schema": "slop_ninja.admission.error/v1",
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
| `not_registered` | 422 | no | the hotkey has no UID on the subnet |
| `registration_mismatch` | 422 | no | the opening names another registration generation |
| `commitment_not_found` | 409 | yes | no logged commitment yet (none, or not yet final) |
| `commitment_before_open` | 422 | no | the commitment was made before the cohort opened |
| `commitment_mismatch` | 422 | no | the opening does not open the hotkey's current commitment (`detail.payload_cohort` when parseable), or (at the cutoff) the commitment shares its extrinsic with another hotkey's (`detail.reason` `shared_extrinsic`) |
| `object_too_large` | 413 | no | `artifact_len` exceeds `max_object_len` or `max_parts` parts |
| `second_artifact` | 409 | no | a session already exists for this commitment, or another live session for this hotkey and commitment (`detail.session`) |
| `part_out_of_range` | 422 | no | `n` is not in `1..=part_count` |
| `part_length_mismatch` | 422 | no | the part is not exactly its expected length |
| `part_digest_mismatch` | 422 | yes, resend | the streamed part does not match its digest |
| `session_not_uploading` | 409 | no | the session no longer accepts parts, completion or an abort |
| `stale_attempt` | 409 | no | the abort or reopen names another attempt than the current one (`detail.attempt`) |
| `parts_missing` | 409 | yes, upload them | completion before every part (`detail.missing`) |
| `rate_limited` | 429 | yes | per-hotkey request rate exceeded (not yet enforced) |
| `paused` | 503 | yes | the coordinator is paused or catching up; the cutoff extends |
| `unavailable` | 503 | yes | temporary failure |
| `internal` | 500 | yes | unexpected failure |

Session refusals (in `session.refusal`):

| Code | When | Meaning |
| --- | --- | --- |
| `object_mismatch` | completion | the assembled object's SHA-256 or length is not the envelope's |
| `artifact_invalid` | completion | `ArtifactReader` refused the object (`detail.reason`) |
| `contract_mismatch` | completion | the header's contract is not the cohort's output contract |
| `content_mismatch` | completion | the object's derived content is not the committed content |
| `check_failed` | completion | an artifact check refused it (`detail.check`) |
| `commitment_overwritten` | cutoff | a later commitment before the cutoff block replaced the one this session opens |
| `incomplete_at_cutoff` | cutoff | the session was not completed before the close |
| `not_registered`, `registration_mismatch` | cutoff | the registration changed before the cutoff |

Session states: `uploading`, `verifying`, `verified`, `refused`, `admitted`,
`overflow`, `superseded`, `aborted`.

The admission refusals of the announce-readiness plan map onto these codes. A
session refusal appears in the hotkey view's `sessions`; a refused session
request appears in its `last_refusal`.

| Case | Code |
| --- | --- |
| no commitment, or not yet finalized | `commitment_not_found` |
| wrong cohort, chain or hotkey | `wrong_cohort`, `commitment_mismatch`, `wrong_chain`, `hotkey_mismatch`, `registration_mismatch` |
| overwritten commitment | `commitment_mismatch` (at the session) or `commitment_overwritten` (at the cutoff) |
| a second artifact for the same hotkey | `second_artifact` |
| upload after the cutoff | `after_cutoff`, `incomplete_at_cutoff` |
| object larger than the bound | `object_too_large` |
| graph not allow-listed | `check_failed`: `graph_not_allowed`, `graph_op_not_allowed`, `graph_class` |
| more parameters than the graph class's bound (160M; under spec v3, 173,549,185 for the tower class) | `check_failed`: `too_many_parameters` |
| tokenizer not byte-identical | `check_failed`: `tokenizer_mismatch` |
| role-schema violation | `check_failed`: `schema`, `binding`, `file_role` |
| custom-op domain or external data | `check_failed`: `graph_custom_domain`, `graph_external_data` |
| undeclared files | `check_failed`: `undeclared_file` |

## 9. Privacy and retention

- Objects and openings stay in the private evaluation bucket and are never
  readable through this API. Only admitted entrants' artifacts and openings are
  published, after the cohort closes, in the signed `CohortClosure`.
- Refused, overflow, superseded and aborted uploads are deleted once the
  roster freezes (no later than the close) and are never published: the
  object, the content-addressed slices the upload's own copies created and
  the parts manifest it wrote, except what another upload still uses (the
  slice store is shared by digest).
- Session records and last refusals are content-free (digests, lengths,
  states, codes, positions) and stay readable at least until the cohort closes;
  after that the published package is the record.
- The upload is protected in transit by TLS, to an origin the client
  authenticates as section 4 describes. Encryption to the coordinator key is a
  later, separately qualified layer around the same bytes.
- On the miner's machine, the object and the openings are files of mode 0600.

## 10. Public reads the client depends on (planned public layout)

`status` and `next-cohort` read the public status snapshot, unauthenticated.
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
  "next": { "cohort": 8, "earliest_open_epoch": 23189 },
  "coordinator": "https://…",
  "active_target": "<hex or null>",
  "reference": "<hex or null>"
}
```

The snapshot is unsigned, and the client labels it so. It is information only:
its `coordinator` field becomes an upload destination only when the trust file
pins that origin (section 4). The coordinator's `GET /admission/v1/cohort` is
authoritative for uploads; the snapshot is a published mirror and can lag it.

## 11. Not in v1

- Presigned URLs, S3 API tokens or any credential other than the hotkey.
- Carrying a commitment, a queue place or an uploaded object to a later cohort.
- Per-coldkey limits or a random or stake-weighted draw under the roster cap
  (a user decision; it would get a new `selection` name).
- Encryption of the upload to the coordinator key.
- A coordinator signature over the cohort response and the status snapshot; v1
  authenticates the destination by its pinned origin and TLS.

## 12. Notes for the coordinator implementation

- Serialize session operations and the close in one Durable Object per cohort,
  persist part records (number, length, digest, R2 part tag) and each session's
  attempt before answering, and keep the multipart upload id in that state so
  completion can resume after a restart.
- Stream bodies; never buffer a part or the object. Parse routes with
  `Route::parse` and the headers in place from the request bytes. Keep every
  bound (`part_size`, `max_parts`, `max_object_len`, `request_validity_blocks`,
  rate limits) as a versioned parameter, not a constant, and keep responses
  within `response_limit`.
- Keep the commitment log of section 2 on the finalized cursor: per commitment
  event, `CommitmentOf(netuid, who)`, `Uids` and `BlockAtRegistration` at that
  block's hash, plus the subnet's registration events, within the shared RPC
  budget. Session opening and the close read only the log.
- Record one `last_refusal` per hotkey and cohort, overwritten by the next
  refusal, after the signature verifies.
- Answer `GET /admission/v1/cohort` with `cache-control: no-store`, and put the
  cursor in `request_expired`'s `detail.finalized_block`.

## 13. Implementation notes (coordinator, testnet)

- **Where.** The routes are the eval Worker's, served only when the
  deployment opens them (`ADMISSION_ROUTES`: `off` by default, which answers
  404; `hotkeys`, only to the listed hotkeys, for operator drills; `open`).
  The Worker verifies each request's signature and body digest before
  forwarding, so a request that fails them never reaches the cohort
  authority (`EvalCohort`), which authenticates against the cursor, decides
  and records in one transaction, then creates, aborts or completes the R2
  multipart upload and records the result. Session operations, the close
  and the freeze are serialized by that one object.
- **Parameters.** `part_size`, `max_parts`, `request_validity_blocks`,
  `roster_cap`, `selection`, the artifact spec (id and digest), the
  operator's own hotkeys and the fresh pool are the plan's `admission` block.
  The deployment's `ROSTER_CAP` bounds `roster_cap`. Coordinator-opened
  cohorts take them from the standing template. The artifact spec is spec v2
  until admission runs name the cohort's spec (other specs are refused at
  the plan, `admission_spec_unsupported`).
- **Outside entrants.** An admitted entrant whose hotkey is not one of the
  plan's `operator_hotkeys`, and whose content is not an already published
  artifact's, is an outside entrant (its artifact loaded and scored the
  control document). Fresh human stock is drawn only for a cohort whose
  frozen roster holds one, from the plan's fresh pool; a plan with a fresh
  pool must list the operator's hotkeys; every other cohort draws only
  already-exposed stock.
- **Commitment log.** It has its own share of the coordinator's per-minute
  RPC budget. A commitment whose value does not decode, whose extrinsic also
  committed for another hotkey, whose block state the node no longer keeps,
  or that a runtime other than the stored one wrote, is never logged; a
  session that opens it gets `commitment_not_found`, and the miner commits
  again. A snapshot block whose state is gone pauses the cohort for the
  operator.
- **Retention.** Objects of refused, overflow, superseded and aborted
  sessions, and the slices and manifests only their copies created, are
  deleted after the freeze. Admitted objects stay private until the close;
  `closure-artifacts.json` beside the signed closure names them for
  publication.
- **Not yet.** `rate_limited` is not enforced (the signature check in the
  Worker and the `hotkeys` allow-list keep unsigned and unlisted load off
  the cohort authority, the unsigned cohort read excepted). The digest-stream
  part check is not qualified, so parts record their claimed digest.
  Admission runs use the scorer lane's probe route: it loads by spec v2,
  with the probe's load wait rather than the cohort's allowance.

# slop-ninja-admission-worker

The key-free upload Worker of Slop Ninja testnet admission:
`slop-ninja-admission-testnet`, served at `https://testnet-admission.slop.ninja`.
It takes every untrusted request of the admission contract
`slop_ninja.admission/v1` ([admission API](../../docs/testnet/admission-api.md))
and holds no signing key: no chain hotkey, and no package, receipt or
authorization key.

| Binding or secret | What |
| --- | --- |
| `ADMISSION_TESTNET` (R2) | the private upload bucket `admission-bucket-testnet`: uploaded objects only |
| `EVAL_ADMISSION_TESTNET` (service) | the eval Worker's internal admission interface |
| `ADMISSION_INTERFACE_TOKEN` (secret) | the token both Workers present on the internal interfaces |

## Routes

- `/admission/v1/...` (public): every route of the contract. Before it
  forwards anything, the Worker runs one front sequence
  (`slop_ninja_admission_gate::decide`, which the miner client's mock also
  runs):
  1. parses the route (every path segment bounded by its format; a hotkey
     segment by the SS58 shape);
  2. verifies the sr25519 signature over the request message (CPU only);
  3. in `hotkeys` mode, checks the signer is listed;
  4. runs the header-only checks: a hotkey segment must decode as SS58 and
     name the signer, a creation must carry the upload media type, a
     bodiless route's signed digest must be the empty string's, a declared
     `Content-Length` must be within the route's bound (one
     `SubmissionUpload` record for a creation, none for a bodiless route),
     and a part's must be present and no larger than the largest object any
     admission spec allows;
  5. looks the signer up in the isolate's roster (`not_registered` when
     absent): the subnet's hotkeys at the finalized head,
     `SubtensorModule.SubnetworkN(601)` and `Keys(601, uid)` for every uid
     below it, read with `state_queryStorageAt` once a block per isolate (12
     s), by one request at a time while later requests wait for it. The keys
     are composed by name from live V15 metadata and read together with
     `System.LastRuntimeUpgrade`, which rebuilds the layout when the runtime
     changes; no version is compared. After a failed load the isolate
     answers `unavailable` for one block without reading again;
  6. reads the body within its route's bound and checks it against
     `x-sn-content-sha256` (a part's body is not read: it streams to R2).

  `valid_until` is the cohort authority's to check, against its own cursor
  (the Worker's chain head may run ahead of it). The Worker then forwards
  one bounded message to the eval Worker. A part runs in three arena scopes:
  the sequence and the cohort authority's `part-begin`, the stream (one
  page, while the body streams through a `FixedLengthStream` of the part's
  exact length into the upload bucket's `uploadPart`), then `part-record`;
  the record carries the signed request again, and the cohort authority
  re-derives the target from the session. A body of another length answers
  `part_length_mismatch`.

  The unsigned cohort read is served from the cohort snapshot the eval
  Worker writes to the upload bucket at every heartbeat
  (`cohort/v1/current.json`): the Worker keeps it in the edge cache for one
  block under one key for every URL variant (the query is dropped) and
  answers `no-store`, with `paused: true` once the snapshot is older than
  the coordinator's heartbeat freshness. It never reaches the eval Worker.

  A signed request costs no RPC call of its own: the roster's reads (two
  head reads, one marker and count read, and one `Keys` read per few
  hundred uids) happen once a block per isolate, and after a runtime
  upgrade one metadata read per isolate. There is no rate cap (decision
  D6).

  Answers outside the handler (an arena refusal, an internal error) and a
  body the platform could not read are the contract's retryable
  `unavailable` with `Retry-After`. An eval answer that refuses the
  forwarded message itself (400 or 413) is `bad_request`; any other eval
  failure is `unavailable`.
- `POST /internal/upload/v1/frame`: only for the eval Worker's framing step,
  which addresses the internal host `admission-upload.internal` through its
  service binding and presents the token (the route class, and so the
  framing footprint, is given only to `POST` requests for that host). It
  streams one uploaded object through
  `slop_ninja_artifact_check::admission::Framer` (one 1 MiB buffer, at most
  one byte past the declared length) and answers a `FrameOutcome`. A failed
  object read answers 503 with `x-sn-frame-failure: object-read`, and a
  framing request this Worker's platform could not take (its body unread,
  an arena refusal) 503 with `x-sn-frame-failure: platform`: the eval Worker
  retries both without counting them. A request that does not decode is
  400. A request for the probe
  key `probe` answers `{"probe": "ok"}` without reading anything (the eval
  Worker's `GET /eval/admission/link`).
- Anything else answers 404.

`ADMISSION_ROUTES`: `open` (decision D6), `hotkeys` with
`ADMISSION_HOTKEYS` for drills, or `off`. The tracked value is `hotkeys`
until the deployed path has been drilled; the drill deploy passes the test
hotkeys with `--var`, and the switch to `open` is its own commit and deploy.

A part's stream scope has the Worker's arena deadline: 15 minutes,
borrowed from the longest documented Workers wall limit (a fetch itself has
none). A part of 64 MiB (the testnet plan's `part_size`) streamed slower
than about 0.6 Mbit/s outlasts it: the stream scope (one page) then counts
as abandoned while it streams and recovers when the stream ends (the arena
runs in `observe` here, so this is counted only).

## Memory

The one export, `fetch`, is hand-written as the coordinator's are: a glue
ticket, the arena clock, then the handler in its own bounded scope with the
route's footprint (`src/routes.rs`), under this crate's
`memory-model.json` (unmeasured, so the arena runs in `observe`). The
roster is a 2 MiB static array (every UID a `u16` can name), static data
below the pool, so it never holds arena pages. Bodies are
read with the coordinator's bounded readers (`cf_io.rs`, shared as source).

## Checks

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --lib -- -D warnings
```

The miner client's `tests/e2e_upload_worker.rs` runs the client against its
mock fronted by the same front sequence this Worker runs
(`slop_ninja_admission_gate::decide`); that crate's tests count the I/O
each refusal costs. Deploy order and probes:
`subnet/coordinator/TESTNET.md`, "Upload Worker: key-free admission front".

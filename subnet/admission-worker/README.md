# slop-ninja-admission-worker

The key-free upload Worker of SlopNinja testnet admission:
`slop-ninja-admission-testnet`, served at `https://testnet-admission.slop.ninja`.
It fronts every public route of the admission contract
`slop_ninja.admission/v2` ([admission API](../../docs/testnet/admission-api.md))
and holds no signing key and no R2 binding: no chain hotkey, no package,
receipt or authorization key, and no object-store credential. No upload byte
passes through it: miners PUT the artifact header and each file straight to
R2 under presigned URLs, which the eval Worker obtains from the internal
signer Worker (`subnet/r2-signer`).

| Binding or secret | What |
| --- | --- |
| `EVAL_ADMISSION_TESTNET` (service) | the eval Worker's internal admission interface |
| `ADMISSION_INTERFACE_TOKEN` (secret) | the token both Workers present on the internal interfaces |

## Routes

- `/admission/v2/...` (public): every route of the contract. Before it
  forwards anything, the Worker runs one front sequence
  (`slop_ninja_admission_gate::decide`, which the miner client's mock also
  runs):
  1. parses the route (every path segment bounded by its format; a hotkey
     segment by the SS58 shape);
  2. verifies the sr25519 signature over the request message (CPU only);
  3. in `hotkeys` mode, checks the signer is listed;
  4. runs the header-only checks: a hotkey segment must decode as SS58 and
     name the signer, a creation must carry the upload media type, a
     bodiless route's signed digest must be the empty string's, and a
     declared `Content-Length` must be within the route's bound (one
     `SubmissionUpload` record for a creation, none for every other route);
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
     `x-sn-content-sha256`.

  `valid_until` is the cohort authority's to check, against its own cursor
  (the Worker's chain head may run ahead of it). The Worker then forwards
  one bounded message to the eval Worker (`/internal/admission/v2/<op>` for
  `create`, `read`, `urls`, `complete`, `repair` and `hotkey`) and relays its
  reply byte for byte (the body is passed on as raw JSON, never parsed into
  a tree).

  The unsigned cohort read is served from the Worker's edge copy of the
  cohort snapshot, kept for one block under one key for every URL variant
  (the query is dropped). On a miss it asks the eval Worker's snapshot
  operation (`/internal/admission/v2/snapshot`), which answers from the
  snapshot object the cohort authority writes to the eval bucket at every
  heartbeat without reaching the cohort object. The same copy carries the
  gate's repair list (never served to clients): a session read or `repair`
  by a signer the roster does not pass goes on only when the list names it. The client's answer is `no-store`, with
  `paused: true` once the snapshot is older than the coordinator's heartbeat
  freshness. A snapshot that does not parse is never cached.

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
- `POST /internal/upload/v2/header`: only for the eval Worker's header
  check, which addresses the internal host `admission-upload.internal`
  through its service binding and presents the token (the route class, and
  so the header footprint, is given only to `POST` requests for that host).
  The body is the header bytes the eval Worker read from R2 after R2
  verified them against the envelope's `header_sha256` (at most the
  protocol's 1 MiB; a declared length above it is refused, 413, before the
  body is touched), and `x-sn-artifact-spec` names the cohort's spec. The
  Worker decodes and checks it with
  `slop_ninja_artifact_check::admission::check_header_bytes` (the declared
  file count is refused before anything is decoded; no file byte is read
  and nothing is hashed) and answers 200 with the `HeaderOutcome`, whatever
  its verdict. An unknown spec answers 400. A body this Worker's platform
  could not read, or an arena refusal of the route, answers 503 with
  `x-sn-header-failure: platform`, which the eval Worker retries without
  counting it. The body `probe` answers `{"probe":"ok"}` without decoding
  anything (the eval Worker's link check).
- Anything else answers 404.

`ADMISSION_ROUTES`: `open` (decision D6), `hotkeys` with
`ADMISSION_HOTKEYS` for drills, or `off`. The tracked value is `hotkeys`
until the deployed v2 path has been drilled; the drill deploy passes the
test hotkeys with `--var`, and the switch to `open` is its own commit and
deploy.

## Memory

The one export, `fetch`, is hand-written as the coordinator's are: a glue
ticket, the arena clock, then the handler in its own bounded scope with the
route's footprint (`src/routes.rs`), under this crate's
`memory-model.json` (unmeasured, so the arena runs in `observe`). Every
footprint is derived from a format: the public route's from the densest JSON
tree of one 64 KiB roster RPC reply and four 64 KiB values (the contract's
response limit, the RPC reply bound, the forwarded message), the header
route's from the protocol's 1 MiB header limit. Bodies are read with the
coordinator's bounded readers (`cf_io.rs`, shared as source), which admit
each body's exact block before copying it in. The roster is a 2 MiB static
array (every UID a `u16` can name), static data below the pool, so it never
holds arena pages.

## Checks

```sh
cargo test
cargo test --features arena-witness arena_witness -- --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo clippy --target wasm32-unknown-unknown --lib -- -D warnings
```

The witnesses run each route's heaviest path at its bound under
`Mode::Strict` in exactly its footprint: the header route over the worst
header shapes (a contract string filling the 1 MiB body, the longest names,
a valid table, garbage, a count above the table), the public route over the
densest roster reply, a full roster call, a relayed reply at the interface
bound and the snapshot at its bound. From `subnet/`, `cargo test --test
no_object_hash` and `--test no_runtime_gates` scan this crate too.

The miner client's `tests/e2e_upload_worker.rs` runs the client against its
mock fronted by the same front sequence this Worker runs
(`slop_ninja_admission_gate::decide`); that crate's tests count the I/O
each refusal costs. Deploy order and probes:
`subnet/coordinator/TESTNET.md`, "Upload Worker and direct upload
(admission v2)".

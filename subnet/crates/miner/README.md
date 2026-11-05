# Miner client (`slop_ninja_miner`)

`slop_ninja_miner` takes an *A* artifact from model files to an admitted
entrant on testnet subnet 601. It is Rust on rtao at the workspace's pin
(`fa83052d`), with no btcli or Python. It follows the admission contract in
[`docs/testnet/admission-api.md`](../../../docs/testnet/admission-api.md),
whose byte-level helpers live in [`wire/`](wire) (crate
`slop-ninja-admission-wire`; no I/O, and its arena-safe items build for
`wasm32-unknown-unknown`, for the coordinator to share). The admission checks
are the *A* artifact spec's
([`docs/testnet/artifact-spec.md`](../../../docs/testnet/artifact-spec.md)),
run through the artifact-check crate ([`../artifact-check`](../artifact-check)).

Platforms: Linux and macOS (Unix file modes protect openings and objects).
Built and tested on Linux x86_64 only; macOS is untested.

The [testnet miner guide](../../../docs/testnet/miner-guide.md) covers the
whole path around this client: the wallet and hotkey and registration on
subnet 601 with the rtao command line tool, training with the
[training guide](../../../docs/testnet/training-guide.md), and what is live.

## What an artifact is

An artifact is a directory with exactly these six files, under the output
contract `slop_ninja_full_a_contract_v2`:

| File | Role |
| --- | --- |
| `a-fp32band.onnx` | graph |
| `heads.json` | weights |
| `contract.json` | output schema |
| `calibration.json` | calibration |
| `manifest.json` | owner metadata (the export record; not part of the committed content) |
| `tokenizer.json` | tokenizer (byte-identical to the pinned one) |

`pack --dir` takes the names, roles and contract from the spec, so a miner
never types them. Any other file in the directory is refused
(`undeclared_file`).

## Local paths

The JSON role files are published with the artifact when its cohort's scoring
starts.
A provenance string that reads as an absolute local path (`/…`, `~/…`, a
Windows drive or UNC path), or carries a home directory or a file URL inside
it, would publish your directory layout, including your user name, and a
relative path under an ignored `data/` tree would publish its private
directory names, so `pack` and `check` warn about it, and about object keys
that do (`src/provenance.rs`).
The scan streams each JSON file and reports JSON pointers only (array
positions as `*`, a path-like key as `<path-like key>`), listing them while
their bytes fit the file's length and counting the rest; a file over its
role's runtime bound, or one that cannot be read, is reported as not scanned.
It reads the start of each string and a few embedded forms, so it can miss a
path written another way: it is a check, not a guarantee. The coordinator
admits such an artifact all the same. The repository's exporters and
normalization tools record portable paths at the provenance fields they write:
repository-relative, or only a final component under the ignored `data/` tree
or outside the repository, never a user or home directory name
(`grammar/crates/grammar-eval/python/provenance_paths.py`,
`grammar_a_baseline::provenance`). A warning means a file was written another
way, by an older revision, or carries a path in free text. Re-export, or
rebuild the normalization and contract with relative paths before training,
and pack again.

## Commands

| Command | What it does |
| --- | --- |
| `pack --dir MODEL_DIR --out OBJ` | Writes the protocol's artifact object `u32_le(len(H)) \|\| H \|\| files` with mode 0600 (it must stay secret until the cohort's scoring starts), hashing each file exactly once, in the pass that writes it: the header's length is fixed before any digest is known, so room for it is reserved, each file is streamed into place while it is hashed, and the header goes in last. From pack on, that pack hash is the only SHA-256 anyone computes over the model's bytes (the repository's graph exporter, run before pack, still checks its own output against the class's structure digest). `--file ROLE:PATH ... [--contract C] [--root DIR]` packs any file list instead. It prints the header's SHA-256 and length, and warns on stderr about each JSON file holding strings or keys that read as or carry a local path (see above), naming their JSON pointers; the object is written all the same, also when the scan cannot run. |
| `check OBJ [--coordinator URL]` | Reads the object's header (a range read) and runs the artifact spec's admission checks on the files in place, hashing none of them: the file table, the tokenizer's pinned digest and length, the role schemas with the export manifest's digests compared against the header's, the ONNX pre-parse (custom domains, external data, operator allow-list), the graph class (a byte comparison of the graph's structure stream with the class's reference stream, which the build ships; design decision D5) and the parameter bound. A file changed on disk since `pack` is caught by R2 at upload (`BadDigest`), not here. A refusal prints the code the coordinator would report in `check_failed`; an object whose header does not decode, or whose length is not its header's, is refused as `object`. With a coordinator it also checks the cohort's contract, object bound and spec. A third checker, `slop_ninja_miner.provenance/v1`, reports `[WARNING] absolute_path` for each JSON file holding strings or keys that read as or carry a local path (`"warning": true` in `--json`), and a `not_scanned` note when it cannot scan; it never refuses and never leaves the check incomplete. It reads the cohort from `--coordinator`, from the public snapshot's coordinator, or from the one origin the trust file pins. Exit 0 pass, 1 refused, 3 incomplete: no coordinator given, no cohort admitting and no next plan named in the cohort read (between windows it checks against the next cohort's plan values when the read names them, with a `next_cohort` note), a coordinator that cannot be chosen or read (a warning, and the reason in the `cohort_unknown` note; the local checks still run), or a cohort that uses another spec than this build. |
| `commit --artifact OBJ` | Reads only the object's header, the admitting cohort from the coordinator and the hotkey's registration from finalized chain state, makes a fresh nonce and the opening (it commits to the content the header derives), stores it with mode 0600, then signs and submits `Commitments.set_commitment(netuid, [Raw44(payload)])` with the hotkey and reads it back at finality. `--dry-run` shows the opening and the call and stores, signs and submits nothing. |
| `upload --artifact OBJ [--opening FILE]` | Opens the session, PUTs the header straight to R2 with the URL the session returns, asks for the file URLs (the coordinator checks the header first), PUTs each missing file straight to R2 (largest first), completes the session and waits for verification. No upload byte goes to the coordinator. Rerun to resume: per object, never per byte. Without `--opening` it uploads the local opening whose payload is the commitment the coordinator observed, or else the newest one confirmed on chain. A refused envelope stays refused: commit again (a new nonce) to upload another object. |
| `repair --artifact OBJ [SESSION] [--cohort N]` | For a session whose recorded objects went missing on the coordinator's side (its `repair_needed` list; a platform fault, never the miner's), asks for fresh URLs for exactly those objects and PUTs them again from the local object, in any phase while they are still needed. The session is found by full id, a unique prefix, or, with no id, the one that needs repair in the cohort. The object's header must be the session's. |
| `status` | The public snapshot (unsigned) or, without `--public`, the calendar from the coordinator's unsigned cohort read, local openings and the coordinator's view of the hotkey: registration, the commitment that counts, sessions with full ids and recorded objects, every session refusal, objects to repair, and the last refused session request (for example `second_artifact`, with the live session). `--repair --artifact OBJ` also repairs, as `repair` does. It names the state directory when it holds no opening, and says which parts it skipped for want of endpoints or a named hotkey. The local openings are always printed: a cohort read or snapshot that fails is reported after them (exit 2). |
| `next-cohort` | The admitting cohort and its planned cutoff, or the next cohort, as subnet epoch indices, each with its distance from the confirmed epoch: from the public snapshot with `--public`, else from the coordinator's unsigned cohort read (`GET /admission/v2/cohort`) at `--coordinator` or the trust file's one pinned origin, measured from its `confirmed_epoch` (absolute epochs when the coordinator reports none). |

### Flags and environment

Every flag below is also in `slop_ninja_miner <command> --help`.

| Flags | Commands | Meaning |
| --- | --- | --- |
| `--coordinator URL` (`SLOP_NINJA_COORDINATOR`) | `check`, `commit`, `upload`, `repair`, `status`, `next-cohort` | coordinator admission base URL; when the trust file pins origins, it must be one of them |
| `--public URL` (`SLOP_NINJA_PUBLIC`) | the same | public host with the unsigned status snapshot |
| `--trust FILE` (`SLOP_NINJA_TRUST`) | the same | the release trust file, whose `admission_origins` pin the coordinators and whose optional `upload_host` pins the R2 S3 host of the presigned URLs |
| `--http-timeout-secs N` (300) | the same | bound on each request without an object body |
| `--wallet-path DIR` (`SLOP_NINJA_WALLET_PATH`, default `~/.bittensor/wallets`), `--wallet NAME` (`SLOP_NINJA_WALLET`), `--hotkey NAME` (`SLOP_NINJA_HOTKEY`), `--hotkey-file PATH` | `commit`, `upload`, `repair`, `status` | the hotkey |
| `--state-dir DIR` (`SLOP_NINJA_MINER_HOME`) | `commit`, `upload`, `status` | openings and upload progress |
| `--dir DIR`, or `--file ROLE:PATH ...` with `--contract C` and `--root DIR`; `--out OBJ` | `pack` | the model files and the output object |
| `--contract C`, `--max-object-len N`, `--json` | `check` | expected contract and size bound (default: the cohort's); JSON reports |
| `--artifact OBJ` | `commit`, `upload`, `repair`, `status --repair` | the packed object |
| `--cohort N` | `commit` | require this cohort to be the admitting one |
| `--cohort N` | `repair`, `status` | the cohort to ask about (default: the admitting one) |
| `--network NAME` (`test`), `--rpc URL ...`, `--rpc-per-minute N` | `commit` | rtao network label, RPC endpoints (default: the network's) and a client-side request budget |
| `--replace`, `--dry-run`, `--deadline-secs N` (600) | `commit` | commit again for a cohort that already has an opening; show without storing, signing or submitting; bound on signing, submission and finality |
| `--opening FILE` | `upload` | the opening to upload |
| `--attempts N` (6), `--min-upload-kib-per-sec N` (32), `--quiet` | `upload`, `repair`, `status --repair` | attempts per retryable request and per object; the slowest rate this client waits for on a PUT (each PUT's local timeout is `--http-timeout-secs` plus its size at this rate; nothing about the rate is sent, since R2 checks a URL's expiry when the PUT starts); no progress lines |
| `--wait-secs N` (600), `--poll-secs N` (10) | `upload` | how long to wait for verification, and how often to read the session meanwhile |
| `--stop-after-objects N` | `upload` | stop after N object PUTs in this run; run `upload` again to resume |

### Exit status

| Exit | Meaning |
| --- | --- |
| 0 | success; for `check`, every check ran and passed |
| 1 | refused: an admission check (`check`), the upload session (`upload`), or a request the coordinator refused with a code that retrying cannot change (for example `not_registered` for a signed `status` read) |
| 2 | error: usage, configuration, network or file, a check the client makes before it signs anything, a PUT R2 refused for a reason no retry fixes (`BadDigest`, `SignatureDoesNotMatch`), a repair that could not finish, or a retryable answer still there after the attempts; nothing was decided |
| 3 | `check` refused nothing but could not run every check |

## Where uploads go

The signed session requests carry the opening, with its secret nonce. The
client sends them only to:

- an origin listed in `admission_origins` of the release trust file
  (`--trust testnet-trust.json`, `SLOP_NINJA_TRUST`); when that file pins
  origins, every other origin is refused; or
- the origin named with `--coordinator` (`SLOP_NINJA_COORDINATOR`), when no
  trust file pins one.

The testnet release's trust file pins one origin,
`https://testnet-admission.slop.ninja` (the coordinator's key-free upload
Worker). With the trust file and neither `--coordinator` nor `--public`, the
client (every command that talks to the coordinator, `check`, `status` and
`next-cohort` included) uses that one pinned origin; this is the supported path until the
public status host is live. `--coordinator https://testnet-admission.slop.ninja`
names the same origin explicitly. Once the status host is live, `--public`
alone also chooses it. The public snapshot (`--public`, `SLOP_NINJA_PUBLIC`)
is unsigned: its `coordinator` field is used only when the trust file pins
that origin. An edge error page without a contract body (502 to 504, or a
Cloudflare 52x) is retried as `unavailable`. The
client prints the origin before it opens a session, never follows redirects,
and accepts plain HTTP only for loopback hosts. Response bodies are bounded by
the contract's limit.

**The artifact's bytes go straight to R2** under presigned URLs the
coordinator mints for the session's owner, one `PutObject` for the header and
one for each distinct file. Before it sends a byte, the client checks each
URL (`slop_ninja_r2_presign::check`): `https`; the trust file's pinned
`upload_host` when one is set, else an R2 S3 host
(`*.r2.cloudflarestorage.com`); exactly the key it derives itself from the
cohort, the commitment and the object's digest; exactly the seven SigV4
parameters with the four signed headers. It then sends exactly three headers
computed from its own committed length and digest (`content-length`,
`if-none-match: *`, `x-amz-checksum-sha256`), never another `x-amz-*` header,
`content-encoding` or chunked transfer encoding. R2 hashes the body as it
arrives and stores nothing on a mismatch. A miner therefore needs outbound
HTTPS to the coordinator origin and to the R2 S3 host. Each URL names the
storage account's S3 host, the bucket and the signing key's id; none of these
is a credential.

The hotkey is `--wallet NAME --hotkey NAME` under `--wallet-path` (default
`~/.bittensor/wallets`) or `--hotkey-file PATH`; only `commit` (without
`--dry-run`), `upload`, `repair` and `status` unlock it. Openings and upload
progress live in the state directory (`--state-dir`, `SLOP_NINJA_MINER_HOME`,
else `$XDG_DATA_HOME/slop_ninja_miner`, `~/.local/share/slop_ninja_miner`, or
`~/Library/Application Support/slop_ninja_miner` on macOS), in directories of
mode 0700 and files of mode 0600. A file that group or others can read is
refused. Opening files are schema `slop_ninja.miner.opening/v2`: they name the
object by its header's SHA-256 and length. A v1 file (which named a
whole-object digest) still loads, matches its object by the committed
content, and is rewritten as v2 by `upload` once the object's header is
known.

## Rules the client enforces

- An opening names one cohort. `commit` refuses a second opening for the same
  cohort without `--replace`, because committing again overwrites the on-chain
  commitment and moves the hotkey to the new position; the refusal says whether
  the existing opening was confirmed on chain. Every commitment gets a fresh
  nonce from the operating system.
- The opening is stored before anything is signed, so a commitment that lands
  is never without its opening. A dry run stores nothing.
- `set_commitment` goes out as a raw call composed here and pinned with rtao's
  `expected_call`, so rtao signs exactly those bytes. rtao applies no spend or
  netuid limit to raw calls; the chain charges no fee for this one and limits
  each account's commitment space per epoch.
- An overflow entrant cannot carry its opening to the next cohort: `upload`
  refuses an opening for another cohort than the admitting one, and `status`
  says to commit again.
- **Hash once.** Each file is hashed once, at `pack`. `commit`, `upload`,
  `repair` and `check` read the header and take its digests; no command reads
  the object back to hash it. A file changed on disk since `pack` is refused by
  R2 (`BadDigest`), and the client says to re-pack from the same sources (the
  commitment binds the digests); a changed object length is caught before any
  request.
- The session id is the envelope's protocol digest, so a restarted upload finds
  its session from the coordinator alone. The coordinator's records are
  authoritative; the local state file also remembers which objects R2 accepted,
  so a resumed upload goes to `complete` for those instead of sending them
  again, and sends again any that `complete` lists missing.
- Retries are per object, each from byte 0 with a fresh URL: a transport error,
  a 5xx or 429 with doubling backoff; an expired URL at once, declaring the
  achieved rate so the next URL's life covers it. 412 means R2 already holds the
  object (`complete` confirms it). `BadDigest` and `SignatureDoesNotMatch` stop
  the run. Expiry never ends a session: every URL request mints new ones.
- Transport: one 1 MiB buffer per PUT, streamed from disk with an exact
  `content-length`, so memory does not grow with the artifact; a PUT's timeout
  grows with its size (`--min-upload-kib-per-sec`); progress lines on standard
  error give each object's bytes, rate and time left, and the run's total.

## Tests

No test connects to a chain, signs a transaction or calls Cloudflare.

- `tests/golden.rs`: the payload and commitment against the protocol fixture
  lines `submission.chain_payload` and `submission.commitment`, and the
  `set_commitment` call bytes rtao composes against the recorded spec-486
  metadata (`fixtures/commit-call-486.txt`; regenerate with
  `SLOP_NINJA_BLESS_COMMIT_CALL=1`).
- `tests/support/mock.rs`: a local mock of the contract v2 and of R2's
  presigned PUT. The coordinator verifies signed requests with rtao's strict
  sr25519 check, keeps each hotkey's commitment log, mints URLs with
  `slop_ninja_r2_presign` under a test credential and the URL policy's life,
  settles the header from the stub's stored object, records objects at
  `complete`, verifies through `SubmissionUpload::admit_header` and an
  optional artifact-check stage over the stored files (hash-free), answers
  `repair`, and closes the cohort. The R2 stub recomputes the SigV4 signature
  over the request's `host`, `content-length`, `if-none-match` and checksum
  header, refuses unsigned `x-amz-*` headers, `content-encoding` and chunked
  bodies, expires URLs against a mock clock, refuses a body whose SHA-256 is
  not the signed checksum (`BadDigest`, nothing stored) and an existing key
  (412). Faults inject R2 status codes, expiry, a byte changed in transit and
  dropped connections; coordinator modes inject `paused`, `rate_limited` with
  `Retry-After`, damaged URLs, lost objects, redirects and an oversized
  response.
- `tests/e2e_mock.rs`: pack (byte-identical to the protocol's builder), check,
  commit preparation (opening v2), an interrupted and resumed upload through a
  transient R2 failure with progress counted, the exact headers of every PUT,
  re-signing after `request_expired`, admission at the commitment position, and
  the refusals: no commitment, a commitment before the window, an overwritten
  commitment (at the session and at the cutoff), a second artifact and a newer
  commitment superseding it, wrong cohort, a forged signature, an object over
  the bound (refused locally), an upload incomplete at the cutoff, requests
  after the cutoff, and overflow followed by a fresh commitment for the next
  cohort; a v1 opening matched by content and migrated in place.
- `tests/e2e_direct.rs`: URLs refused before any byte (another host, another
  key, an extra query parameter, another checksum, a pinned host); the URL
  policy's life and expiry (a fresh URL at once); resume per object
  and 412 from another machine; a changed object stopped by `BadDigest` with
  nothing stored, and a changed length caught before any request; dropped
  connections, 429 and 5xx retried until the attempts run out; a refused
  signature and a 501 `NotImplemented` stopping at once; redirects never followed; `objects_missing` at
  completion; repair of lost objects after the close, refused for another
  object and not needed once publication is recorded.
- `tests/e2e_cases.rs`: `wrong_chain`, `hotkey_mismatch`,
  `registration_mismatch`, `not_registered` and `commitment_mismatch` as the
  hotkey's last refusal; `contract_mismatch`, `content_mismatch`,
  `artifact_invalid` and `check_failed` (with the artifact spec's own code,
  equal to the local `check`'s; a header refusal before any file is sent);
  `paused` and `rate_limited`; a replayed `urls` request minting URLs only for
  the same keys; the cutoff reading the commitment log when a hotkey commits
  again after the cutoff block; redirects never followed; oversized responses
  refused; sessions usable only by their hotkey; a returning miner waiting for
  its new commitment to be logged.
- `tests/e2e_upload_worker.rs`: every signed request through the upload
  Worker's own gate, no upload byte through it.
- `tests/cli.rs`: the binary with a test wallet hotkey: pack (file list and the
  spec's directory), check exit codes with spec codes, the absolute-path
  warning at pack and check (text and `--json`), `status` with nothing to show,
  next-cohort with epochs relative to the confirmed epoch, the trust file
  pinning the coordinator, a resumed upload with progress lines, a refused
  second artifact shown by `status`, the choice of opening, `repair` by id
  prefix and `status --repair` after a loss, the endpoint override refused off
  loopback; the trust file alone choosing the origin for `check`, `status` and
  `next-cohort`, a refused signed `status` read (`not_registered`) exiting 1,
  and `status` showing the local openings during a coordinator outage before it
  exits 2.
- `subnet/tests/no_object_hash.rs` (`the_miner_hashes_each_file_once_at_pack`):
  `pack`'s writer holds exactly one hasher, per file, inside the loop that
  writes it; no miner source hashes anything else but the fixed-size envelope's
  signed body digest.
- Unit tests in `src/` and `wire/src/tests.rs`.

From the `subnet/` workspace:

```
cargo test -p slop_ninja_miner -p slop-ninja-admission-wire
cargo test --test no_object_hash --test no_runtime_gates
```

The hidden `--r2-endpoint-override URL` flag sends PUTs to a plain-http
loopback origin (a local R2 stub) for these tests; any other host is refused.

No small example artifact passes `check`: the graph class fixes the graph at
601,028,257 bytes of a real export, with the pinned tokenizer and role files
that agree with it. Synthetic objects reach only refusals, as these tests use
them; the artifact-check crate's own tests run R0 through the same checks.

Not qualified here: the live `commit` submission path, R2's behaviour on real
presigned PUTs (design gates G0 to G13), and an upload to the real
coordinator, whose admission v2 routes are being built on the same branch.

## Operator tool: `slop_ninja_reference_stage`

Not a miner command. The operator stages a reference package (`R_k`) with it
(`slop_ninja_reference_staging_v1`): after `slop_ninja_a_served_profile pack --spec <spec>
--model-dir <dir> --out <dir>`, `slop_ninja_reference_stage --eval-url <origin>
--operator-token-file <file> --dir <dir> --spec <spec> [--trust <file>]
[--out <record>]` asks the eval Worker for one presigned PUT per file, checks
each URL as `upload` does (the pinned R2 host and bucket, exactly the key
`reference/v2/files/<sha256>`, the seven SigV4 parameters and the three
headers), streams each file from disk (R2 verifies its SHA-256), then asks the
eval Worker to pin every file's entity tag and write the package manifest. It
prints the package digest and manifest key. The token is sent only as the
`Authorization` header; URLs are never printed.


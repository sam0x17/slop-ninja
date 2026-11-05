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

The JSON role files are published with the artifact after its cohort closes.
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
| `pack --dir MODEL_DIR --out OBJ` | Streams the six files into the protocol's artifact object `u32_le(len(H)) \|\| H \|\| files`, refuses a file that changes while it is packed, verifies the result with the protocol's `ArtifactReader`, and writes it with mode 0600 (it must stay secret until the cohort closes). `--file ROLE:PATH ... [--contract C] [--root DIR]` packs any file list instead. It warns on stderr about each JSON file holding strings or keys that read as or carry a local path (see below), naming their JSON pointers; the object is written all the same, also when the scan cannot run. |
| `check OBJ [--coordinator URL]` | Verifies the object and runs the artifact spec's admission checks, the streaming checks the coordinator runs: the file table, the tokenizer byte match, the role schemas, the ONNX pre-parse (custom domains, external data, operator allow-list), the graph class and the parameter bound. A refusal prints the code the coordinator would report in `check_failed`; an object that does not parse is refused as `object`. With a coordinator it also checks the cohort's contract, object bound and spec. A third checker, `slop_ninja_miner.provenance/v1`, reports `[WARNING] absolute_path` for each JSON file holding strings or keys that read as or carry a local path (`"warning": true` in `--json`), and a `not_scanned` note when it cannot scan; it never refuses and never leaves the check incomplete. It reads the cohort from `--coordinator`, from the public snapshot's coordinator, or from the one origin the trust file pins. Exit 0 pass, 1 refused, 3 incomplete: no coordinator given (no `--coordinator`, no `--public` and no trust file that pins one origin), no cohort admitting and no next plan named in the cohort read (between windows it checks against the next cohort's plan values when the read names them, with a `next_cohort` note), a coordinator that cannot be chosen or read (a warning, and the reason in the `cohort_unknown` note; the local checks still run), or a cohort that uses another spec than this build. |
| `commit --artifact OBJ` | Reads the admitting cohort from the coordinator and the hotkey's registration from finalized chain state, makes a fresh nonce and the opening, stores it with mode 0600, then signs and submits `Commitments.set_commitment(netuid, [Raw44(payload)])` with the hotkey and reads it back at finality. `--dry-run` shows the opening and the call and stores, signs and submits nothing. |
| `upload --artifact OBJ [--opening FILE]` | Opens the session, uploads missing parts, completes and waits for verification. Rerun to resume. Without `--opening` it uploads the local opening whose payload is the commitment the coordinator observed, or else the newest one confirmed on chain. `--restart` aborts a refused attempt (for example `object_mismatch`) and sends every part again. |
| `abort [SESSION] [--cohort N]` | Aborts this hotkey's session: by full id, by a unique prefix, or, with no id, the live session in the cohort. Needed before uploading another object for the same commitment. |
| `status` | The public snapshot (unsigned) or, without `--public`, the calendar from the coordinator's unsigned cohort read, local openings and the coordinator's view of the hotkey: registration, the commitment that counts, sessions with full ids, every session refusal, and the last refused session request (for example `second_artifact`, with the live session to abort). It names the state directory when it holds no opening, and says which parts it skipped for want of endpoints or a named hotkey. The local openings are always printed: a cohort read or snapshot that fails is reported after them (exit 2). |
| `next-cohort` | The admitting cohort and its planned cutoff, or the next cohort, as subnet epoch indices, each with its distance from the confirmed epoch: from the public snapshot with `--public`, else from the coordinator's unsigned cohort read (`GET /admission/v1/cohort`) at `--coordinator` or the trust file's one pinned origin, measured from its `confirmed_epoch` (absolute epochs when the coordinator reports none). |

### Flags and environment

Every flag below is also in `slop_ninja_miner <command> --help`.

| Flags | Commands | Meaning |
| --- | --- | --- |
| `--coordinator URL` (`SLOP_NINJA_COORDINATOR`) | `check`, `commit`, `upload`, `abort`, `status`, `next-cohort` | coordinator admission base URL; when the trust file pins origins, it must be one of them |
| `--public URL` (`SLOP_NINJA_PUBLIC`) | the same | public host with the unsigned status snapshot |
| `--trust FILE` (`SLOP_NINJA_TRUST`) | the same | the release trust file, whose `admission_origins` pin the coordinators |
| `--http-timeout-secs N` (300) | the same | bound on each request without a part body |
| `--wallet-path DIR` (`SLOP_NINJA_WALLET_PATH`, default `~/.bittensor/wallets`), `--wallet NAME` (`SLOP_NINJA_WALLET`), `--hotkey NAME` (`SLOP_NINJA_HOTKEY`), `--hotkey-file PATH` | `commit`, `upload`, `abort`, `status` | the hotkey |
| `--state-dir DIR` (`SLOP_NINJA_MINER_HOME`) | `commit`, `upload`, `status` | openings and upload progress |
| `--dir DIR`, or `--file ROLE:PATH ...` with `--contract C` and `--root DIR`; `--out OBJ` | `pack` | the model files and the output object |
| `--contract C`, `--max-object-len N`, `--json` | `check` | expected contract and size bound (default: the cohort's); JSON reports |
| `--artifact OBJ` | `commit`, `upload` | the packed object |
| `--cohort N` | `commit` | require this cohort to be the admitting one |
| `--cohort N` | `abort`, `status` | the cohort to ask about (default: the admitting one) |
| `--network NAME` (`test`), `--rpc URL ...`, `--rpc-per-minute N` | `commit` | rtao network label, RPC endpoints (default: the network's) and a client-side request budget |
| `--replace`, `--dry-run`, `--deadline-secs N` (600) | `commit` | commit again for a cohort that already has an opening; show without storing, signing or submitting; bound on signing, submission and finality |
| `--opening FILE`, `--restart` | `upload` | the opening to upload; abort a refused attempt and send every part again |
| `--attempts N` (6), `--min-upload-kib-per-sec N` (32) | `upload` | attempts per retryable request; slowest rate a part may take |
| `--wait-secs N` (600), `--poll-secs N` (10) | `upload` | how long to wait for verification, and how often to read the session meanwhile |
| `--stop-after-parts N` | `upload` | stop after N parts in this run; run `upload` again to resume |

### Exit status

| Exit | Meaning |
| --- | --- |
| 0 | success; for `check`, every check ran and passed |
| 1 | refused: an admission check (`check`), the upload session (`upload`), or a request the coordinator refused with a code that retrying cannot change (for example `not_registered` for a signed `status` read) |
| 2 | error: usage, configuration, network or file, a check the client makes before it signs anything, or a retryable answer still there after the attempts; nothing was decided |
| 3 | `check` refused nothing but could not run every check |

## Where uploads go

An upload sends the opening, with its secret nonce, and the whole artifact
before the cohort closes. The client sends them only to:

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

The hotkey is `--wallet NAME --hotkey NAME` under `--wallet-path` (default
`~/.bittensor/wallets`) or `--hotkey-file PATH`; only `commit` (without
`--dry-run`), `upload`, `abort` and `status` unlock it. Openings and upload
progress live in the state directory (`--state-dir`, `SLOP_NINJA_MINER_HOME`,
else `$XDG_DATA_HOME/slop_ninja_miner`, `~/.local/share/slop_ninja_miner`, or
`~/Library/Application Support/slop_ninja_miner` on macOS), in directories of
mode 0700 and files of mode 0600. A file that group or others can read is
refused.

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
- The session id is the envelope's protocol digest, so a restarted upload finds
  its session from the coordinator alone. A part is skipped only when the
  coordinator holds it with the local bytes' digest. Abort and reopen name the
  session's attempt, so a replayed abort cannot undo a later upload.
- Transport: one part in memory at a time, shared across retries; a part's
  timeout grows with its size (`--min-upload-kib-per-sec`, default 32); the
  coordinator's `Retry-After` sets the retry wait; `parts_missing` uploads the
  listed parts and completes again.

## Tests

No test connects to a chain or signs a transaction.

- `tests/golden.rs`: the payload and commitment against the protocol fixture
  lines `submission.chain_payload` and `submission.commitment`, and the
  `set_commitment` call bytes rtao composes against the recorded spec-486
  metadata (`fixtures/commit-call-486.txt`; regenerate with
  `SLOP_NINJA_BLESS_COMMIT_CALL=1`).
- `tests/support/mock.rs`: a local mock of the contract. It verifies signed
  requests with rtao's strict sr25519 check, keeps each hotkey's commitment log
  (so the cutoff counts the last commitment before the cutoff block), sessions
  with attempts, parts, verification through `ArtifactReader`,
  `SubmissionUpload::admit` and an optional artifact-check stage, the last
  refused request per hotkey, and the cutoff. Modes inject `paused`,
  `rate_limited` with `Retry-After`, lost part records, a part changed in
  transit, the fallback that records claimed part digests, redirects and an
  oversized response.
- `tests/e2e_mock.rs`: pack, check, commit preparation, an interrupted and
  resumed upload with a transient failure, re-signing after `request_expired`,
  admission at the commitment position, and the refusals: no commitment, a
  commitment before the window, an overwritten commitment (at the session and
  at the cutoff), a second artifact, abort and reopen, wrong cohort, a forged
  signature, an object over the bound, an upload incomplete at the cutoff,
  requests after the cutoff, and overflow followed by a fresh commitment for
  the next cohort.
- `tests/e2e_cases.rs`: `wrong_chain`, `hotkey_mismatch`,
  `registration_mismatch`, `not_registered` and `commitment_mismatch` as the
  hotkey's last refusal; `contract_mismatch`, `content_mismatch`,
  `artifact_invalid` and `check_failed` (with the artifact spec's own code,
  equal to the local `check`'s); `paused` and `rate_limited`; `parts_missing`;
  parts changed in transit with and without the digest-stream path, and
  `--restart`; replayed aborts and reopens refused as `stale_attempt`; the
  cutoff reading the commitment log when a hotkey commits again after the
  cutoff block; redirects never followed; oversized responses refused; sessions
  readable only by their hotkey.
- `tests/cli.rs`: the binary with a test wallet hotkey: pack (file list and the
  spec's directory), check exit codes with spec codes (an unreadable file is an
  error, an unparseable object is refused as `object`, and an unreachable
  coordinator still runs the local checks), the absolute-path warning at pack
  and check (text and `--json`), `status` with nothing to show,
  next-cohort with epochs relative to the confirmed epoch, the trust
  file pinning the coordinator, a resumed upload, a refused second artifact
  shown by `status`, abort by prefix and by cohort, and the choice of opening;
  the trust file alone choosing the origin for `check`, `status` and
  `next-cohort` (the cohort read's calendar, before and after a close, and
  `check` against the next cohort's plan values between windows), a refused
  signed `status` read (`not_registered`) exiting 1, and `status` showing the
  local openings during a coordinator outage before it exits 2.
- Unit tests in `src/` and `wire/src/tests.rs`, including a check that every
  error code and state is documented in the contract with its HTTP status.

From the `subnet/` workspace:

```
cargo test -p slop_ninja_miner -p slop-ninja-admission-wire
```

No small example artifact passes `check`: the graph class fixes the graph at
601,028,257 bytes of a real export, with the pinned tokenizer and role files
that agree with it. Synthetic objects reach only refusals, as these tests use
them; the artifact-check crate's own tests run R0 through the same checks.

Not qualified here: the live `commit` submission path and an upload to the
real coordinator, whose admission routes are not implemented yet.

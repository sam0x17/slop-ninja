# A Container server

`a-container-serve` serves the *A* [release candidate](../../docs/a-baseline-release.md)
in f32 on a CPU Container. It is the server qualified in
[f32 serving of A on Cloudflare Containers](../../docs/a-container-serving.md).
It takes text and returns the release-contract prediction: origin probabilities,
calibrated confidence, share and conditional attribution. It makes no outbound
network calls and logs no text, token ids or request ids.

- **Tokenizer:** the release `tokenizer.json`, CLS…SEP, no truncation. Inputs
  over 2,048 tokens return `unsupported_length` and no prediction; empty text
  returns `empty_text`.
- **Encoder:** the banded f32 ONNX graph through ONNX Runtime 1.31.1 (`ort`
  2.0.0-rc.10, dynamic loading). Graph optimisation level 3, one inter-op
  thread, batch 1, padded to a multiple of 64 with a masked pad. Only the
  graph's `h` output is used.
- **Heads:** the release f64 heads, calibration and attribution runtime. The
  crate compiles `full_a.rs`, `full_a_spans.rs`, `involvement_calibration.rs`
  and `shared.rs` from
  [`grammar-a-baseline`](../../grammar/crates/grammar-a-baseline) and refuses
  to start if their digests differ from the served profile's pins.
  `full_a_spans.rs` is compiled because `full_a.rs` uses it; the served
  package is R0 (export v2, no token head), so no sentence or span is
  returned.

## Routes

| Route | Result |
| --- | --- |
| `GET /healthz` | 200 with `ready: true` once the model is loaded and the graph digest matches; otherwise 503, with `error` set after a failure. |
| `GET /info` | Profile id, expected and verified graph digest, load timings, CPU, memory and kernel facts. |
| `POST /v1/predict` | Body `{"id", "text", "diagnostics"?}`, at most 4 MiB. Returns `id`, `profile_id`, `text_sha256`, `status`, `token_count`, `padded_to`, `token_ids_sha256_le_u32`, `prediction` and `timing_ms`. `diagnostics: true` adds raw outputs, `h` and the graph outputs. Returns 503 while loading, 500 after a failed load or digest check, and 400 for a bad request. |

At most 32 requests are in flight; further requests get 503 `busy`.
Environment: `A_HOST` (`0.0.0.0`), `A_PORT` (8080), `A_SESSIONS` (1–8, default 1),
`A_THREADS` (1–16, default all CPUs), `A_MODEL_DIR` (`/model`) and `A_ORT_LIB`
(`/opt/ort/libonnxruntime.so.1.31.1`).

## Served profile

[`profiles/a-baseline-rc-s17-f32band.json`](profiles/a-baseline-rc-s17-f32band.json)
(schema `slop_ninja_a_served_profile_v1`) records the SHA-256 and size of every
served file, and the server checks it before reporting ready:

| File | SHA-256 | Source |
| --- | --- | --- |
| `a-fp32band.onnx` | `08848d95…ca4cb7` | f32 banded export of `model/encoder.safetensors` and `model/heads.json` |
| `heads.json`, `contract.json`, `calibration.json`, `manifest.json` | as in the release | release `model/` |
| `tokenizer.json` | `6c8aaa9a…c08d30` | release `runtime-base/` |

It also records the release package's `RELEASE.json`, `SHA256SUMS`, replay
receipt and encoder digests, the export tool and its gate records, the heads
runtime sources, and the ONNX Runtime library and image digests.

**Heads runtime pins.** The pins name the current `grammar-a-baseline`
sources, not the ones the release replay receipt binds (`full_a.rs`
`3d991945…`, `involvement_calibration.rs` `a2e036c3…`). They were requalified
on October 31, 2026: builds from both source sets, including the deployed
testnet scorer binary, return bit-identical results on 81 requests, and the
outputs stay within the release replay gate against the current FP64 runtime
([heads runtime update](../../docs/a-container-serving.md#heads-runtime-update)).
A build from these sources has a new binary digest, so the next scorer image
deploy must update `SCORER_BINARY_SHA256`.

**Release `SHA256SUMS` gap.** The release package's `SHA256SUMS` covers every
package file and passes `sha256sum -c`. It does not cover the served graph,
which is exported from the release weights outside the package by a
Python/torch tool (`torch.onnx.export`, opset 17). Two independent exports gave
the same digest. The served profile closes the gap:

- it binds the graph's digest and size to the release `SHA256SUMS` digest and
  to the release files the graph was derived from;
- `slop_ninja_a_served_profile verify --release-dir` checks that the package's
  `SHA256SUMS` has the recorded digest and gives every served release file its
  recorded digest;
- the server hashes the whole graph at startup, in parallel with session
  creation, and does not report ready until it matches.

The release package itself is unchanged; its records keep their digests.

## Build and check

Tests and builds run on `build-host` as one Cargo job under the repository's
cargo lock. The crate is its own workspace with a locked dependency set.

```sh
cargo test --locked --manifest-path inference/a-container-serve/Cargo.toml
cargo build --release --locked --manifest-path inference/a-container-serve/Cargo.toml
```

The tests are offline and load no model. They check the tracked profile, the
heads runtime digests, digest and size refusal, the release `SHA256SUMS`
binding, the single-use policy, the memory model and the spec's value budget
derived from it, the adversarial set's graph rewrites and the load guards.
The route tests (`tests/routes.rs`) start the built `a-scorer` on loopback:
the work directory is cleared at start and core dumps are off; a manifest past
the JSON budget or with a free-text `profile_id` is refused 422 before any
chunk, and a load before every chunk is written is refused; a bound instance
refuses every other package, cohort and part with `instance_bound`; and no
document text or id reaches standard error, `/healthz` or a refusal. With
`A_R0_MODEL_DIR` and `A_ORT_LIB` they also load R0 through the routes, score
sentinel documents, check each result's keys against the content-free schema
and that no value but the id carries the input, check that a ready instance
refuses another cohort or package, and run `slop_ninja_a_local_score` and
check that its summary carries no input. The engine tests (`tests/engine.rs`) run synthetic artifacts
through ONNX Runtime: a small graph with the scorer's graph contract whose
outputs are known in closed form, a word-level tokenizer and heads of each
schema. They compare every engine output with the heads runtime on the same
values, for document outputs, the heads v2 token head on `token_states` and
the heads v3 token head on `span_tower_token_states`, and check batch
invariance and the refusal of a graph without the states its heads read.
They also check that results are content-free for each heads form, with and
without spans, including a text with no words whose span emission fails.
They need `A_ORT_LIB` (the ONNX Runtime 1.31.1 library); without it they
print a notice and pass without loading a graph. The crate's dependency on the
artifact checker (`subnet/crates/artifact-check`) is a path dependency, so the
image build needs the checkout's `subnet/` and `grammar/` directories, as it
already needs `grammar/`.

`slop_ninja_a_served_profile` checks files against the compiled-in profile:

```sh
slop_ninja_a_served_profile verify --model-dir MODEL_DIR --release-dir RELEASE_DIR
slop_ninja_a_served_profile sums > MODEL_DIR/SHA256SUMS   # then sha256sum -c in MODEL_DIR
slop_ninja_a_served_profile show
```

**Image.** [`container/Dockerfile`](container/Dockerfile) builds from a context
assembled in ignored storage, never in Git:

- `ort/libonnxruntime.so.1.31.1`;
- `model/` with the six served files, checked with `verify --model-dir`;
- `bin/a-container-serve`, built in `rust:1.99.1-slim-bookworm` (pinned by
  digest in the profile). The checkout is mounted read-only so that the
  `grammar-a-baseline` and artifact checker sources are visible; the binary
  goes to a separate target directory.

The image build context (with the model) stays in ignored storage. Building
or pushing an image and any Cloudflare deployment each need the operator's
approval.

## a-scorer: batch scoring by artifact digest

`a-scorer` (`src/bin/a_scorer.rs`) is the hidden-evaluation scorer behind the
on-demand `a-scorer-testnet` Container. Its image
([`container/scorer.Dockerfile`](container/scorer.Dockerfile), 86 MB) holds the
ONNX Runtime library and the binary only; no model.

- **Any admitted artifact.** An artifact is the six files of the
  [artifact spec](../../docs/testnet/artifact-spec.md). The files manifest v2
  (`slop_ninja_a_artifact_files_v2`, `src/artifact.rs` and
  `slop_ninja_artifact_check::files_manifest`) lists each file's name, role,
  size and SHA-256 (the header's, which R2 verified when the miner uploaded
  it) and where it is stored (bucket, key, entity tag); its exact bytes are
  the package, and their SHA-256 is the `package_sha256` an artifact record
  commits to. The coordinator moves each file in chunks of at most 64 MiB
  (`MAX_RANGE_BYTES`, ranged reads pinned to the entity tag), which the
  scorer writes at their offsets. No file is hashed on the load path: the
  digests are the header's (`subnet/tests/no_object_hash.rs` scans it).
- **Admitted by the cohort's spec.** Each load admits by the artifact spec it
  names by digest (`spec=`, the digest `slop_ninja_artifact_check spec`
  prints and a cohort's `artifact_spec` names), which is its cohort's. The
  scorer admits by spec v2, v3 or v4 (`engine::admits_by`; spec v1 bounds no
  JSON file), and a load that names no spec is admitted by spec v2
  (`engine::UNNAMED_LOAD_SPEC`), the spec testnet cohorts name. Spec v3
  admits every artifact spec v2 admits (every artifact
  spec v1 admits whose JSON files fit the JSON budget, plus span-head
  artifacts) and span tower artifacts, and spec v4 (`engine::NEWEST_SPEC`)
  admits exactly what v3 admits, stating the graph class rule as the byte
  comparison, so a tower-class artifact is admitted
  only by a load that names spec v3 or v4. `slop_ninja_a_served_profile pack
  [--spec v2|v3|v4] --model-dir DIR --out DIR` admits a model directory by the
  spec `--spec` names (the cohort's), else by spec v2 as a load that names
  none is, and writes its files and manifest. The manifest's `profile_id`
  names that spec, so the package digest depends on it; for R0's own files it
  keeps the served profile's `profile_id` under either spec, so R0's deployed
  package digest is unchanged.
- **Admission inside the scorer.** Before it takes a chunk, the scorer checks
  the manifest against the load's spec's file table and bounds
  (`src/limits.rs`): the six names and roles, the graph at the exact length of
  a class that spec serves, the pinned
  tokenizer, the heads within the heads runtime's limit and the JSON budget.
  Once every chunk is written, and before ONNX Runtime opens anything, it runs
  the spec's whole admission check on the files with the header's digests
  (`engine::admit`), graph pre-parse included; the role binding and the load
  guards compare digests with the header and hash no file, and the graph's
  class is a byte comparison of its structure stream with the class's
  reference stream, compiled into the binary (decision D5). A refusal fails the load with `refused <code>: <detail>`.
- **Guards.** R0's form (export v2, heads v1, no span head) keeps the release
  guard (`engine.rs` `guard`, unchanged and pinned by spec v1). Every other
  form takes the shared load guard (`src/load_guard.rs`), which spec v2 and v3
  pin and the artifact checker compiles as well.
- **Span heads.** The export and heads schemas declare which token states the
  heads read, never a runtime version: a span head or a heads v2 token head
  (the S0+ form) reads graph output `token_states`, the encoder's final-norm
  states; a heads v3 token head (the spans-S2 S2c form) reads
  `span_tower_token_states`, the span tower's states. The load refuses a graph
  that lacks an output its heads read. Admission serves the span class (with
  `token_states`) and, under spec v3, the tower class (with
  `span_tower_token_states`). ONNX Runtime computes every graph output, so a
  tower-class artifact pays for the tower on every document, spans asked or
  not. The heads runtime's four pinned files are unchanged; the engine also
  compiles the tower runtime (`full_a_tower.rs`), pinned separately
  (`TOWER_RUNTIME_SHA256`), and the artifact checker pins the same digest
  (spec v3 `tower_runtime_sha256`).
- **Single use.** The first accepted load binds the process to its package,
  cohort and spec for its life, whatever the load's outcome
  (`src/instance.rs`). A prepare or load of another package, cohort or spec
  and a score request for another package or cohort get 409
  `instance_bound`. Each file is written once in place, and at start the
  process removes the work directory's `files/`, so a process
  restarted in the same container holds no earlier artifact. The cohort half
  binds only when the caller names it: a load without `cohort=` binds the
  absent cohort, which every later request without one matches. A cohort id
  follows the coordinator's cohort label rule (1 to 96 bytes of
  `[A-Za-z0-9-]`). `/healthz` reports `"single_use": true` and the binding,
  its spec included.
  The coordinator's lane client (`subnet/coordinator/src/cf_lanes.rs`, with
  its pure parts in `src/eval/scorer_client.rs`) reads `/healthz` before it
  uses a running instance and destroys one bound to another package or
  cohort, or whose load of this binding failed, without counting a start
  failure; it names its cohort label on load and score when the scorer
  reports single use, and compares packages only otherwise. A
  `refused <code>` reply (422 at load, or a failed slot's `error`) is a
  definitive artifact failure, except a failed slot's `refused object`: the
  checker also files an instance's failed read of its assembled files, or a
  failed allocation while buffering one, under `object`. That and every other
  load failure count only on a freshly started instance.
- **Routes.**
  - `POST /v2/prepare?package=<sha256>[&cohort=<id>][&spec=<spec sha256>]`
    takes the files manifest v2, checks it against the load's spec
    (`limits::check_manifest`) and preallocates each file at its exact length:
    201 with the chunks still `missing` when it binds, 200 with them when the
    same binding is already receiving (or loading, loaded), 400 for a malformed
    query (`query`) or a spec digest the scorer does not admit by (`spec`),
    neither of them the artifact's fault, 422 when the manifest is refused
    (including `refused profile_id` for a profile id no packer writes:
    `<spec id>/<graph class id>` or the served profile's id), 409
    `instance_bound`, 507 `insufficient_memory` when the container's memory
    limit is below the [memory model](#memory-model)'s bound, or a placement
    failure (both platform faults, not the artifact's).
  - `PUT /v2/files/<index>/<offset>` writes one chunk of the manifest's
    `index`th file at `offset` (a multiple of 64 MiB inside the file, at most
    64 MiB, exactly the chunk's length): 201 when written, 200 with nothing to
    store once the binding is loading or loaded (a late retry), 409
    `not_prepared` before a prepare, 422 for a chunk outside the manifest or of
    another length. Nothing is hashed.
  - `POST /v2/load` (the same query) loads the bound artifact in the
    background once every chunk is written: 202 when started, 200 when it is
    already loading or loaded, 409 `missing_chunks` (with the list),
    `not_prepared` or `instance_bound`.
  `POST /v1/score` scores up to 128 documents (`{"package", "cohort"?,
  "spans"?, "docs": [{"id", "text"}]}`) across the session pool and returns,
  per document, the status, text digest, token count, prediction and output
  digest, with process and cgroup CPU time. With `"spans": true` and a token
  head, the prediction also carries `sentences` and `spans`, with
  `span_status` and `span_output_sha256`; the document prediction and
  `output_sha256` are the same with or without spans. `GET /healthz` reports
  `empty`, `loading`, `ready` or `failed`, the binding, the build identity and
  the limits, which list the specs a load may name (`specs`, by id and digest)
  and the one a load that names none gets (`unnamed_load_spec`).
- **Defaults.** 4 sessions of 1 thread (the throughput configuration on
  standard-4), work directory `/tmp/a-scorer`. The heads runtime is checked
  against the served profile's pins at start, and the tower runtime against
  its pin. Core dumps are off.
- **Isolation.** The Worker starts the container with Internet access disabled;
  file chunks arrive from the coordinator's private buckets (a miner's from
  the admission bucket, a reference package's from the eval bucket), piped by
  the coordinator's lane. It logs start, load
  and failure events only.

The engine repeats the release guards except the served profile's base
temperature pin, so artifacts other than the release candidate can load; graph
shape, contract, calibration and role thresholds are still checked.

### Memory model

`src/limits.rs` bounds what one load can use, for any artifact the spec admits
or refuses after assembly (model `slop_ninja_a_scorer_memory_v3`: the v2
model per graph class):

```
bound(class) = base + sessions x coefficient(class) x graph bytes(class) + requests
             + 2 x 268,435,456 + 3 x value budget(class) bytes
```

| Class | Graph bytes | Coefficient | Value budget |
|---|---:|---:|---:|
| plain, span | 601,028,257; 601,028,403 | 2.0075 | 2,029,195,723 |
| tower | 681,508,964 | 2.3083 | 1,540,445,560 |

- **Base and sessions, measured** on a Linux x86_64 host (Intel Raptor Lake,
  AVX2), with ONNX Runtime 1.31.1 from the pinned wheel, as the peak resident
  memory of `slop_ninja_a_local_score` with one intra-op thread per session.
  - Plain and span (November 1, 2026): R0 scoring 15 documents of 14 to 2,048
    tokens. Peak resident memory was 1,565,851,648 bytes with one session and
    5,185,441,792 with four: 1,206,530,048 per session (2.0075 bytes per graph
    byte) and a base of 359,321,600. Each class's bound uses its own graph
    length; spec v2's value budget, which the plain and span classes share,
    is derived at the larger of the two graphs (601,028,403 bytes).
  - Tower (November 2, 2026): the S2c span package with spans requested, on
    the same requests, on 16 documents of 2,040 to 2,048 tokens, on its 215
    synthetic reference cases and on a cohort-sized stream: 1,500 synthetic
    documents of 8 to 2,048 tokens (windows of the package's synthetic cases,
    one in ten of 1,985 tokens or more, every padded length many times), in a
    random order and in ascending length. The stream is not tracked: it was
    drawn with seed 601 from the 215 cases, and its SHA-256 is `bc7a1b60…`
    (`b904f46d…` in ascending length). The peaks (the largest `VmHWM` read
    from `/proc` while each run went on, or the scorer's own reading at its
    end) were at most 1,716,305,920 bytes with one session and 6,651,670,528
    with four (the stream in ascending length; 6,416,629,760 in the random
    order). The coefficient is the smallest with which the base and the
    sessions cover every one of them: (6,651,670,528 - 359,321,600) / 4 /
    681,508,964, rounded up to 2.3083.
- **Requests.** 16 requests in flight, each a score body of at most 8 MiB held
  raw and parsed: 268,435,456.
- **JSON bytes.** Two copies of the spec's JSON byte budget (the admission
  buffers and the guards' reads).
- **JSON values.** Three parses live at once (admission's typed schemas while
  its guard views are built, and the heads the model keeps), each bounded by
  the class's value budget (`max_json_value_bytes`). Admission checks the
  files' value cost before it parses any of them, so a file inside the byte
  budget but costly to parse (many small objects, about 100 resident bytes per
  file byte) is refused `json_value_budget` instead of exhausting memory. Each
  value budget is derived from this model: what a standard-4 instance left the
  process on October 29, 2026 (free memory plus the process's resident set
  with four sessions of R0, 12,078,473,216 bytes) less the other terms at four
  sessions of the class, divided by three. A test recomputes both.
- **Reference structure streams, not reserved.** The graph classes' reference
  structure streams (decision D5) are read-only data compiled into the
  binary, 5,107,056 bytes for the three classes, added after the base was
  measured. A load reads one class's stream, at most its `structure_bytes`
  (1,860,300 for the tower class), once and in order, during the graph walk.
  The pages are clean and backed by the executable, so the kernel can drop
  them under memory pressure and read them again; the bound reserves nothing
  for them. The next measurement of the base takes them in.

| Sessions | Bound (plain, span) | Bound (tower) |
|---:|---:|---:|
| 1 | 7.88 GiB | 6.85 GiB |
| 2 | 9.00 GiB | 8.32 GiB |
| 4 | 11.25 GiB | 11.25 GiB |
| 8 | 15.74 GiB | 17.11 GiB |

The deployed configuration, four sessions on standard-4, fits by
construction for every class: each class's bound is at most what that
instance left the process (`MemTotal` 12,508,404 KiB). The load's limit is
the container's cgroup memory limit when one is visible, else the VM's
`MemTotal`; Cloudflare Containers showed no cgroup limit on October 29, 2026,
so `MemTotal` applies there. A load is refused with 507
`insufficient_memory`, a platform fault the coordinator never attributes to
the artifact, when the limit is below the bound of the class of the
manifest's graph. `/healthz` reports both readings, the limit used and each
class's bound. R0 itself peaked at 5.19 GB with four sessions; its JSON files
cost 5.8% of its value budget, and the S2c package's 11.2% of the tower's.

**The session term is a measurement.** A session's resident memory grows with
the documents it has served and their order, not only with their number, and
the growth slows once every session has met every padded length. On the
1,500-document stream, the S2c package's four-session peak rose by 5 MB over
the last 600 documents in the random order and by 21 MB over the last 500 in
ascending length, and R0's by 6 MB over the last 600 in the random order. A
cohort scores about 1,000 documents per artifact instance; the tower term
covers four-session instances that serve up to 1,500 documents, in the orders
measured. The one-session peak was measured on at most 215 documents.

On November 2, 2026, R0 with four sessions peaked at 5,425,664,000 bytes on
16 documents of 2,040 to 2,048 tokens, at 5,655,351,296 on the 215 synthetic
cases and at 5,801,209,856 on the 1,500-document stream in the random order:
240 MB, 470 MB and 616 MB above the plain class's base and session terms (a
coefficient of 2.2636 would cover the last). The bound still held, inside its JSON terms (R0's files use 5.8% of
their value budget). A plain or span coefficient measured that way would
change spec v2's value budget, so it waits for a later spec version.

### Deployment prerequisites

- **Loads that name spec v3 or v4.** The coordinator's loads (the lane client's
  `load_path` in `subnet/coordinator/src/eval/scorer_client.rs`, which the
  operator probe also uses) name the digest of the spec the cohort's plan
  names (`fanout.score.artifact_spec`), and the operator probe the one its
  request names (`artifact_spec`); a plan or probe that names none is
  admitted by spec v2. The client names a digest only when `/healthz` lists
  it, and compares the binding's `spec` before reusing an instance. A
  tower-class artifact is admitted only by a load naming spec v3 or v4, whose
  bounds (the plan's `artifact_spec`, with its digest frozen in the plan)
  also size the artifact.
- **Rehearsal artifacts and the reference chain.** A load that names no spec
  is admitted by spec v2. The reward-v1 rehearsal
  artifacts were built with non-canonical JSON layouts, R0's export manifest
  and a free-text `profile_id`, so it refuses them (`refused profile_id`; `refused tokenizer_mismatch` for `no-load`,
  whose tokenizer was truncated). Their spec v2 re-encode, kept in the
  operator's private records, has the same values in canonical layouts, the
  export manifest bound to each variant's own files and, in the flattened
  variants, the calibration report's copy of the selected map set to that map
  (provenance only; no prediction reads it). `fail-all` is admitted and fails
  every document; `no-load` is the adversarial `nonfinite-outputs` graph,
  admitted, whose load fails on its warm-up document. The re-encoded package
  and content digests are new, and the main instance's reference chain names
  the earlier rehearsal reference: with this scorer deployed, the main
  instance can run no reward-v1 cohort until that chain record is restarted
  (the chain-record restart makes the released R0, as its path-stripped
  package `143ca2d9…c57f0c`, R_0); the load instance can run the re-encoded
  set.
- **The memory limit.** Read `memory_limit` from a live `/healthz` of the
  deployed instance before relying on the 507 guard; the operator scorer
  probe's reply carries it (`cold.limits`).

## Local scorer

`slop_ninja_a_local_score` (`src/bin/local_score.rs`) runs the hidden scorer's
engine on a miner's own machine: the same admission, guards, graph contract and
heads runtime.

```sh
cargo build --release --locked --manifest-path inference/a-container-serve/Cargo.toml \
  --bin slop_ninja_a_local_score
slop_ninja_a_local_score --model-dir ./my-artifact --docs docs.jsonl --out results.jsonl \
  --ort-lib /path/to/libonnxruntime.so.1.31.1 [--spec v2|v3|v4] [--spans] [--sessions N] [--threads N]
```

`--spec` is the spec the cohort names, the one the hidden scorer admits the
cohort's artifacts by; without it, the newest (spec v4).

Each line of `docs.jsonl` is `{"id", "text"}`, optionally with `"involved"`
(true when AI was involved) and `"share"` (0 to 1). Each output line is the
hidden scorer's per-document result, in input order. Standard error gets a
content-free summary: counts by status and, with labels, the mean confidence
Brier score and the share error over scored documents. The summary is a local
diagnostic, not the cohort score, which uses the cohort's hidden documents,
its source balancing and its failure rules.

ONNX Runtime 1.31.1 is loaded from `--ort-lib` or `A_ORT_LIB`. The hidden
scorer's library is `onnxruntime/capi/libonnxruntime.so.1.31.1` from the
onnxruntime 1.31.1 cp312 manylinux_2_28_x86_64 wheel (SHA-256
`fc44112fbe06d7d689c57e11d3862031f64711007e4ee020f8eed5185fd2c1d3`). The summary
says whether the library given is that one; another build can differ in the
last bits. The crate builds on Linux x86_64 (tested) and needs no Python.

## Adversarial set

`slop_ninja_a_adversarial` (`src/bin/adversarial.rs`, cases in
`src/adversarial.rs`) writes and runs the scorer's adversarial artifact set
from an admitted base artifact; the
[scorer threat model](../../docs/testnet/scorer-threat-model.md) lists the
cases, their required outcomes and the last local run.

```sh
slop_ninja_a_adversarial list
slop_ninja_a_adversarial write --base MODEL_DIR --out CASES_DIR [--case NAME]...
slop_ninja_a_adversarial run --base MODEL_DIR --cases CASES_DIR --docs DOCS.jsonl --ort-lib LIB [--case NAME]...
```

## Span head parity

On November 1, 2026, the span head served through the f32 graph was compared
with the FP64 reference. The artifact was the span export of R0's weights with
the shipped S0+ package's heads, contract, calibration and export manifest,
re-encoded in the spec's canonical layouts (`slop_ninja_artifact_check
canonical`; values unchanged). `slop_ninja_a_local_score --spans --reference`
scored the package's two reference sets with four sessions of one thread, on
the host and library of the memory model, and compared every output with the
package's FP64 Python reference predictions:

| Set | Cases | Sentences / spans | Offsets | Document outputs | Sentence share and confidence | Span share and confidence |
|---|---:|---:|---|---:|---:|---:|
| Synthetic | 15 | 292 / 11 | all equal | 7.0e-6 | 4.7e-6 | 8.8e-7 |
| Local Dev-iter | 200 | 2,278 / 241 | all equal | 5.2e-6 | 9.7e-6 | 7.4e-7 |

The errors are the largest absolute differences on the 0..1 scale. Every
attribution label is equal and every document scored with `span_status`
`ok`. The span counts equal the FP64 replay's (11 and 241). Every error is
inside the 1e-5 gate of the FP64 replay (R1); that gate was set for the FP64
runtime, and these runs are a measurement, not a qualification of a new gate.
Against the package's FP32 Python function, a drift diagnostic, the synthetic
set's largest errors are 1.6e-5 (document), 9.2e-6 (sentences) and 1.6e-6
(spans), with all offsets equal.

The same artifact through `a-scorer`'s HTTP routes (parts, load, score with
`"spans": true`) gave the same results as the local scorer, digest for digest.

**The S2c span package (tower class).** On November 2, 2026, the same
comparison ran for the S2c span package (spec v3, tower class): R0's encoder
and document heads with the S2c tower and token head, calibrated, export v4.
The synthetic set is the package's 215 reference cases; the local set is the
200 Dev-iter cases of a private twin export whose files are byte-identical
to the package's apart from those references.

| Set | Cases | Sentences / spans | Offsets | Document outputs | Sentence share and confidence | Span share and confidence |
|---|---:|---:|---|---:|---:|---:|
| Synthetic | 215 | 2,491 / 208 | all equal | 5.3e-5 | 5.0e-5 | 2.3e-5 |
| Local Dev-iter | 200 | 2,278 / 147 | all equal | 4.8e-6 | 1.9e-4 | 9.9e-6 |

Every attribution label is equal, every document scored with `span_status`
`ok`, and the span counts equal the FP64 replay's (208 and 147). Several
errors are past the FP64 replay's 1e-5 gate; as above, these runs are a
measurement, not a gate. The document outputs are R0's: on the 215 synthetic
cases, the 200 local cases and the 16 container parity requests, the package's
predictions and `output_sha256` equal R0's package's through the same scorer
bit for bit, with spans asked and not, so the document errors are R0's own f32
errors on these documents. Against the package's FP32 Python function, the
synthetic set's largest errors are 6.1e-5 (document), 3.8e-5 (sentences) and
3.5e-6 (spans), with all offsets equal. Through `a-scorer`'s HTTP routes, with
a load that names spec v3, the synthetic set gave the same results as the
local scorer, digest for digest, and the instance kept its single-use binding
(409 for another package, a cohort or a foreign part). A load of the same
package that names no spec, or spec v2, is refused 422 `graph_size`.

**R0 through the generalized scorer.** On the 16 container parity requests
(15 scored, one over the token limit), `a-scorer` built from this source and
the testnet scorer binary it replaced (`9efb681a…`) returned byte-identical
predictions and equal output digests, from the same parts.
`slop_ninja_a_served_profile pack` reproduces R0's deployed package digest
(`2f88b0d2…678559`). The heads runtime pins are
unchanged, so no requalification was due.

## Limits

- **One artifact per server.** `a-container-serve` serves the files of one
  compiled-in profile (R0, no span). `a-scorer` loads one artifact per instance
  by digest and admits it by artifact spec v3; the coordinator's admission
  policy (roster, commitments, cutoff) is not the scorer's.
- **Measured build.** The Cloudflare measurements came from an earlier,
  untracked copy of this server. That copy hashed the graph after readiness
  and only reported the digest. This source refuses a mismatched graph and adds
  `profile_id` and `A_HOST`. Model loading and inference are unchanged.
- **No Worker.** The test Worker was JavaScript and is not tracked. The
  coordinator's Container client must be Rust and must handle start failures
  with bounded retries.
- **Export tool.** R0's graph was exported in Python/torch outside the
  repository; the profile records that tool's digest and gate records. The
  in-repository exporter (`export_a_onnx.py`) reproduces the plain and span
  class structures and writes the tower class's.
- **Span parity.** The span head's and the span tower's outputs through the
  f32 graph are compared with the FP64 reference in
  [Span head parity](#span-head-parity). The live probe route sends no
  `spans` flag, so span outputs have been compared locally only.
- **Platforms.** Tested on x86_64 Linux: Cloudflare Zen 4 with AVX-512, and
  locally with AVX2.

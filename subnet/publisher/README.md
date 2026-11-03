# Publisher (testnet)

A separate Cloudflare cron Worker that turns the coordinator's private state
into static public objects. Once a minute it:

1. reads the coordinator's status through service bindings (the coordinator
   Worker's `/status`, and the eval Worker's `/status` and `/eval/status`,
   each with that Worker's read-only status token), writes the redacted
   status snapshot and evaluates the alert rules, before any cohort work;
2. verifies the next closed `reward-v1` cohort of the public record in the
   private receipts bucket, as a validator would (at most one to completion
   per run). It then copies the cohort's public objects into the separate
   public-candidate bucket byte for byte, verifies the copies again, and lists
   the cohort in the public index;
3. publishes artifact objects from the private eval bucket after their cohort
   closed, default-deny: only an outside entrant's upload with a matching
   admission record, never a reference, an operator's or a copy;
4. writes the cohort index, the leaderboard (JSON and a static page rendered
   from `site/`) and the reference pointers. A pointer links the reference's
   staged reference package (`v1/reference/packages/<sha256>/`, CC BY-SA 4.0)
   when the public bucket holds its parts manifest with that SHA-256; the
   operator stages packages and the publisher never writes under that prefix;
5. copies the current validator authorization and its renewal once their
   cohort is listed;
6. deletes cohort N's detailed receipts from the public host once N+1 and N+2
   are published and N's target is superseded;
7. pings the dead-man heartbeat when enabled, after a run that finished.

The public host serves the bucket's objects directly. The Worker has no
request route, so traffic never runs Worker code and never touches the
spending cap. The object layout is
[`docs/testnet/public-layout.md`](../../docs/testnet/public-layout.md).

**State:** built and tested natively, with a `wasm32` check. Not deployed.
Deployment needs decision D6 (the resources), and the public bucket stays
private until the user turns on public access (D5). The alert sink is a test
sink until D18 picks the channel.

## Layout of the crate

The crate is its own Cargo workspace. It depends on the shared
`slop-ninja-protocol` crate and the admission wire crate by path, so the
records it verifies and the snapshot shape the miner client reads are the
same code.

| Module | Role |
| --- | --- |
| `verify` | cohort verification: signatures, bindings, receipts, published index, recomputed outcome, closure |
| `publish` | one run: discovery, copy, re-verification, artifacts, records, retention |
| `status` | typed status parsing and the public snapshot built from it |
| `alerts` | alert rules, state across runs, sink payloads |
| `index` | cohort index, publication records, reference pointers |
| `leaderboard` | leaderboard document and the static page |
| `redact` | what may leave the private side |
| `arena` | the bounded-memory rules and their measured constants |
| `layout` | every key read or written |
| `config` | vars and secrets; the compiled trust roots |
| `store` | the object-store seam, with an in-memory store for tests |
| `cf` | Workers entry points (`wasm32` only): cron, R2 adapter, status reads, the `PublisherState` Durable Object |

Everything but `cf` is plain Rust and is tested natively over an in-memory
store.

## Safety properties

- **Verify, copy, verify again.** No object is copied before the whole cohort
  verifies. Each copy streams bucket to bucket, and the destination checks
  it against the SHA-256 verified before the copy, so a source that changed
  in between is refused. The cohort is then verified again from the public
  bucket, and is listed only when the copies' digests equal the originals'.
- **Only named objects.** The publisher reads only the cohort objects it
  verifies, and the artifact objects the package binds. The batch shards,
  batch index, results, summaries, probe vectors, plans, pools and
  `keys.json` are never read or copied. Binary records decode canonically and
  completely. The only copied JSON diagnostic (`components.json`) must parse
  as its typed schema with unknown fields refused at every level, agree with
  the manifest and package field for field (its policy document hashing to
  the manifest's `gate_policy` digest) and pass the redaction scan, or it is
  withheld.
- **Typed snapshot.** The status snapshot is built from named fields only.
  Spend, the ledger, generation, models, lanes, jobs, documents and fault
  details never reach it; faults are one boolean. The ledger enters the alert
  rules only as a per-mille ratio.
- **Artifacts are default-deny.** An entrant's artifact is published only
  when the admission route stored its upload envelope (the protocol's
  `SubmissionUpload`, at `ADMISSION_RECORD_KEY`) and that envelope is this
  entrant's: the chain and cohort, the hotkey and registration, the
  committed content, commitment and object, and the nonce the signed closure
  published. Uploading through the route accepts the CC BY-SA 4.0 submission
  terms. Before that check, an artifact is withheld for good when:
  - its hotkey is in `OPERATOR_HOTKEYS` (the owner, our miners and
    validators, synthetic rehearsal identities);
  - its content or object is listed in `PRIVATE_DIGESTS` (the references'
    digests);
  - its content or object is an initial reference (`R_0` or its single
    replacement) that any manifest the publisher opened named, inside or
    outside the record; the index keeps the listed cohorts' ones in
    `initial_references`, so they stay withheld after the manifest chain
    stops naming them. References are published as reference packages
    instead, never as entrant artifacts;
  - it failed the public-copy gate (`public.content`, `public.near.current`,
    `public.near.predecessor`, `public.near.probe`);
  - it joined the near-copy cluster of a withheld artifact.

  Pending artifacts are checked again on every retry. A configuration that
  sets `ARTIFACT_SOURCE_KEY` without `ADMISSION_RECORD_KEY`, a non-empty
  `PRIVATE_DIGESTS` and a non-empty `OPERATOR_HOTKEYS` is refused. `R_k` is
  downloadable only when it is a promoted miner artifact published here.
- **Restarted record.** The public record holds the closed cohorts whose
  manifest opened at or after `PUBLISH_FROM_EPOCH`. Chain epochs only
  increase, so this holds whatever numbering a restarted coordinator uses.
  `RECORD_LABEL_PREFIX` (optional) leaves every other label outside without
  a read. The boundary is decided from the manifest alone, before the
  cohort is verified. Cohorts before it, for another chain, of another
  record kind (load cohorts) or whose manifest does not open are remembered
  and never read again. Unverifiable manifests raise one aggregated alert,
  which a label prefix clears. The record starts at a cohort whose
  reference is `R_0` with no predecessor; until one is listed, later cohorts
  wait. Cohort numbers must order like their opening epochs, or the cohort is
  refused. A refused cohort is not read again by the same build (version and
  trust roots); a new build retries it.
- **Read-only on the private side.** R2 bindings cannot be made read-only,
  so the code writes and deletes only in the public-candidate bucket. The
  tests check that the private buckets see no write or delete.

## Bounded memory

`arena.rs` holds the rules (`slop_ninja.publisher.arena/v2`):

- The Wasm module has fixed linear memory: 64 MiB, initial equal to maximum,
  set in `.cargo/config.toml`, and a test checks that the two values agree.
- Every read is refused before its body is read if it exceeds its bound. The
  bound comes from a signed record already verified: the receipt index from
  the package's receipt count, the bundles from the chunk map, the published
  sets from the manifest's counts. Everything else is bounded by the
  remaining budget.
- One budget per run accounts every held object, every status body and every
  large decoded structure.
- The signed receipt index is authenticated (`Signed::open_raw`) and walked
  in place: its fixed-width entries are checked canonically, sorted and
  unique, and searched by binary search, never decoded into a vector. The
  chunk map is read first, so the peak (the raw index, a bit per receipt and
  the largest bundle) is known before the index is read.
- Status bodies are parsed into typed views without a JSON tree. Each may use
  a third of the remaining budget (the body and a growing buffer's copies),
  reserved while it is held.
- Copies, artifacts included, stream inside the runtime and never enter Wasm
  memory.

`tests/arena.rs` measures the real heap peak of verifying cohorts under a
counting allocator. It checks that the accounting covers that peak.
`arena::max_receipts(per_bundle)` is the largest receipt index the budget
admits when each bundle holds at most `per_bundle` receipts. A cohort over
it is refused (alert `publication_refused`), never partially published.

Measured on 2026-11-01 (native, release build):

| Receipts | Measured heap peak | Accounted peak |
| --- | --- | --- |
| 10,000 | 0.80 MB | 0.77 MB |
| 100,000 | 7.76 MB | 7.73 MB |

The measured peak exceeds the accounted one by the decoded records and the
outcome recomputation, which the test's 2 MiB allowance inside
`RESERVED_BYTES` covers.

- A receipt index entry is 69 bytes and an attempt receipt 151 bytes.
- With one bundle per job, a job holds its tasks times the manifest's
  `max_attempts` receipts. At 2,224 tasks and 3 attempts (6,672 receipts per
  bundle), `max_receipts` is 713,149. A cohort at the plan's roster cap of 40
  (41 jobs with the reference) holds at most 273,552 receipts by
  `arena::cohort_receipts`, so it fits at three attempts per task.
- The real rehearsal cohorts 7, 8 and 9 (1,000, 14,344 and 5,448 receipts)
  verified and published from their exported objects with an accounted peak
  of 1.34 MB at most.
- The bound a plan must stay within is `cohort_receipts(roster_cap, tasks,
  max_attempts) <= max_receipts(tasks * max_attempts)`. The coordinator's
  plan validation does not check it yet (see below).
- The release `wasm32` build links its memory at 1,024 pages (64 MiB),
  initial and maximum alike.
- The `worker-build --release` bundle keeps those limits: its
  `index_bg.wasm` defines one memory of 1,024 pages, initial and maximum
  alike (checked on 2026-11-01).

## Alerts

The rule set is `slop_ninja.publisher.alerts/v2`. The table is in
`alerts.rs`:

- durable faults;
- fallback lasting a tempo;
- a stalled confirmed watermark;
- a work phase past its package deadline;
- scorer, budget and other cohort faults;
- the ledger at 80%;
- two tempos without a coordinator commit;
- a coordinator reveal that differs from its committed row
  (`coordinator_reveal_mismatch`);
- unreachable status;
- refused or stalled publications: cohorts, artifact admission records and
  copies, and the authorization;
- unverifiable manifests (one aggregated alert);
- a refused configuration;
- runs interrupted for a whole tempo.

Each condition has a hold time. It fires once the hold passes, is re-posted
every `ALERT_RENOTIFY_SECS` while it lasts, and posts a resolution when it
clears. A failed post is retried by the next run. A duplicate run posts
nothing new. Posts are content-free. `ALERT_SINK_FORMAT` shapes them for a
generic JSON receiver, a Slack-style `{"text"}` webhook or a Discord-style
`{"content"}` webhook. Events are split into posts within the sink's limit
(2,000 characters for Discord, 40,000 for Slack and for the JSON body), and
each delivered post is acknowledged on its own.

A run first writes the snapshot and evaluates the alerts, using the
publication results of the previous run, then saves its state, and only
then starts cohort work. Each step (a cohort verification, an artifact
copy) renews a two-minute lease and starts only within `RUN_WALL_BUDGET_MS`
(default one cron period). A run killed mid-step blocks the next runs for
one lease at most, and a blocked run still refreshes the snapshot. A refused
configuration still writes a fault snapshot and raises `config_refused`
through whatever sink parses.

The dead-man heartbeat (`HEARTBEAT_URL`, enabled only by
`HEARTBEAT_ENABLED = "true"`) is a GET after each run that finished both
phases. An external service pages when the pings stop.

## Configuration

See `wrangler.toml`. The vars set at deploy are `PUBLISH_FROM_EPOCH`,
`RECORD_LABEL_PREFIX`, `PRIVATE_DIGESTS`, `OPERATOR_HOTKEYS`,
`ARTIFACT_SOURCE_KEY`, `ADMISSION_RECORD_KEY`, `ADMISSION_ORIGIN` and
`NOTICE`. The secrets are `COORDINATOR_STATUS_TOKEN` (the coordinator
Worker's status token), `EVAL_STATUS_TOKEN` (the eval Worker's, which is a
different value), `ALERT_SINK_URL` and `HEARTBEAT_URL`. The trust roots are
the compiled `subnet/coordinator/testnet-trust.json`. The publisher verifies with exactly
the file it publishes, so rotating the roots (D20) means rebuilding the
publisher.

## Authorizations

The validator fetcher reads `v1/authorizations/current.json`
([layout](../../docs/testnet/public-layout.md#authorizations)). The publisher
reads the coordinator's private pointer `authorizations/current.json` in the
receipts bucket, with the same schema and private keys
`authorizations/<sha256>.signed`. It checks each object's digest and length,
opens it under the authorization key, and checks that a renewal renews that
authorization. A launch authorization must name the launch burn target. A
cohort authorization waits until its cohort is listed with the same manifest
and package. The objects are then copied to `v1/authorizations/` and the
public pointer is written.

## Checks

On the project's build host, as a memory-limited job under the shared cargo
lock:

```sh
cargo test --manifest-path subnet/publisher/Cargo.toml
cargo check --manifest-path subnet/publisher/Cargo.toml --target wasm32-unknown-unknown
```

## Not yet done

- Deployment and the live checks:
  - anonymous GETs;
  - one forced drill per alert rule;
  - a heartbeat miss that pages;
  - a stream copy of a full-size artifact (about 610 MB) between buckets.

  Whether R2 accepts an R2 body stream as a put body without a
  `FixedLengthStream` wrapper is the first thing the live copy confirms.
- Coordinator follow-ups (its paths belong to the reward-v1 live stream until
  that branch lands):
  - store each admitted upload's `SubmissionUpload` envelope at the key
    `ADMISSION_RECORD_KEY` names (W3); until then no artifact is published;
  - store signed authorizations and renewals with the private pointer above;
  - refuse plans whose receipt count exceeds the publisher's bound above;
  - a compact status route for the publisher (the active and next cohort,
    the regime, the ledger ratio and a journal summary). `/eval/status` lists
    every cohort ever held, so its size grows with the record; today it is
    about 6 KB a cohort, read within the arena budget;
  - deletion of the private detailed receipts after N+2 (the public copies
    already follow the rule).
- AR-13 gaps:
  - a commit-gap view of validator2: the native validator's journal is not
    visible to the publisher, so only the coordinator's commits are watched;
  - a divergence check between the chain's consensus for subnet 601 and our
    row, which needs chain reads or a heartbeat object from the validators;
    `coordinator_reveal_mismatch` compares only the coordinator's own
    reveal with its committed row.
- Public artifacts are kept once written. Their storage grows with each
  distinct published artifact and is outside the AR-14 spend envelope; a
  retention rule for artifacts that are neither `R_k`, `R_k-1` nor credited
  is a user decision.

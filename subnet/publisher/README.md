# Publisher (testnet)

A separate Cloudflare cron Worker that turns the coordinator's private state
into static public objects. Once a minute it:

1. reads the coordinator's status through service bindings (the coordinator
   Worker's `/status`, and the eval Worker's `/status` and `/eval/status`,
   each with the publisher's own status token for that Worker), writes the redacted
   status snapshot and evaluates the alert rules, before any cohort work;
   once that phase has saved its state, it posts a heartbeat to the alerts
   relay (decision D18, below);
2. copies the validator authorization objects and the coordinator's signed
   row (step 6 below says how), first and on their own: no later step, and
   nothing it fails with, holds a new row or revocation back from
   validators;
3. verifies the next closed `reward-v2` cohort of the public record in the
   private receipts bucket from its signed manifest, its public
   `QuantizedResult` (`result.signed`, `quant-v1`) and its closure (at most
   one to completion per run); it never reads the private score package or
   the components. It then writes only the cohort's allow-listed summary
   (`v1/cohorts/<label>/summary.json`, schema v3: the grid width, per
   entrant in roster order its quantized score `q`, band, gate and
   public-copy gate bits and cluster membership, the cluster named by its
   first member in roster order, `R_k` and `R_k-1` with their `q`, and the
   cutoff and bands on the grid; no exact score and no rank) into the
   separate public-candidate bucket and lists the cohort in the public
   index. The coordinator's signed records, its receipts, receipt index,
   chunk maps and bundles, its published-content and fingerprint indexes
   and its components stay private (`allowlist.rs`, version 3, names every
   public object and field). Cohorts are listed strictly in order: one
   that is incomplete or unreadable for now (a store error, an object not
   stored yet, or a closure or manifest that does not answer during the
   scan) holds every later cohort, so a later cohort never appears first in
   the listing. So does a packaged cohort whose closure the coordinator
   has not written yet (its closure paused or retrying while later cohorts
   close), until something says it never closes: the coordinator's
   retirement marker for it (written only after a closure or an
   abandonment, so with no closure it means the cohort was abandoned; due
   after two later completions, which bounds the hold), or the
   coordinator's signed abandonment record (`abandoned.signed`, named with
   its read bound in the shared contract
   `slop_ninja_admission_wire::receipts`), read within its bound and
   verified (package-key signature under the compiled trust roots, the
   cohort's label, this chain); one that does not verify holds as if
   absent. A cohort this build
   refuses is recorded in the index's `refused` list (label, cohort,
   opening epoch, whether it starts the record, a content-free reason
   naming the object and the kind of failure, and the publisher build; the
   full reason goes to the alert), so the gap is visible, and later cohorts
   are listed after it; a later build reconsiders it and, if it verifies,
   lists it in its place by number (also when it is the cohort that starts
   the record and a later one was listed first). A refused cohort that
   starts the record (reference `R_0`, no predecessor) starts it for later
   cohorts. Before the record has started, a refused cohort that cannot
   start it, or whose manifest did not open, starts nothing: later cohorts
   wait until one that starts it is listed, retired or refused, or a new
   build accepts the refused one;
4. publishes each admitted entrant's artifact at scoring start
   (`src/artifacts.rs`): once a cohort's signed manifest seals its roster,
   the header and files each entrant uploaded directly to the private
   admission bucket are copied to their digest keys in the public bucket
   (`artifacts/v2/<sha256>/header`, `blobs/sha256/<sha256>`), files first,
   never a reference's or an operator's; a closed cohort's record then
   reports what was published, checked against the result's header
   digests. No copy happens at closure;
5. writes the cohort index, the leaderboard (JSON and a static page rendered
   from `site/`) and the reference pointers. A pointer links the reference's
   staged reference package (`v1/reference/packages/<sha256>/`, CC BY-SA 4.0)
   when the public bucket holds its parts manifest with that SHA-256; the
   operator stages packages and the publisher never writes under that prefix;
6. (run at step 2) copies the current validator authorization, any renew-by
   hint refresh, the revocation set and the coordinator's signed row (copied
   byte for byte as soon as they verify: validators commit the row and need
   no cohort record; refused only when malformed: not whole records, a digest
   or length that differs, an entry that does not decode or names another
   chain, a row that does not open under the trust roots or names another
   authorization; a revocation entry that does not open under the trust
   roots is counted and alerted, `revocation_unverifiable`, and validators
   skip it; read bounds are the declared lengths, reserved from the run's
   budget);
7. publishes no detailed receipt, so the public host has no retention rule.
   A first run over a layout v1 index replaces it with an empty v2 index
   (keeping the record prefix, the initial references, the earlier records
   and the cohorts retired before publication) and lists the record's
   cohorts again from their private records, one a run; the operator's key
   audit (`slop_ninja_publisher_ops audit-keys`) deletes what layout v1 left
   in the bucket. The marker's schema and the private authorization pointer's
   key, checks and bounds come from the contract the coordinator also
   imports (`slop_ninja_admission_wire::receipts`);
8. pings an external dead-man URL when one is enabled, after a run that
   finished both phases.

The public host serves the bucket's objects directly. The Worker has no
request route, so traffic never runs Worker code and never touches the
spending cap. The object layout is
[`docs/testnet/public-layout.md`](../../docs/testnet/public-layout.md).

**State:** deployed with no route and no custom domain; public access to
the public-candidate bucket is off until the user turns it on (D5). Decision
D18: alerts go to the private alerts relay Worker
([`subnet/alerts-relay`](../alerts-relay/README.md)) through the
`ALERTS_RELAY_TESTNET` service binding (`ALERT_RELAY = "true"`). The relay
writes each event to its private alert feed, which the operator reads from
outside Cloudflare; email is an optional relay path, off by default. Deploy
the relay before any publisher deploy that carries the binding: Cloudflare
refuses a deploy whose service binding names a Worker that does not exist.

**Alerts live (2026-11-02):** the testnet publisher posts its alerts and its
heartbeat to the relay. The deploy drill:

- `config_warning` (an ignored `ALERT_SINK_URL` deploy-time var on a test
  deploy) fired once and resolved once after the next deploy dropped the
  var. The publisher runs in between posted nothing.
- The relay's `publisher_silent` fired while the earlier publisher build
  sent no heartbeat, and resolved at the first heartbeat.
- `commit_gap` fired for a real condition (the coordinator's signing was
  deferred). One run whose coordinator status read did not answer in time
  resolved it, and the next run fired it again: that build treated an
  unanswered status as a cleared condition. Rule set v4 fixes it (an
  unknown read is never a resolution, below); not deployed yet.

## Layout of the crate

The crate is its own Cargo workspace. It depends on the shared
`slop-ninja-protocol` crate, the admission wire crate and the alerts wire
crate by path, so the records it verifies, the snapshot shape the miner
client reads and the alert posts the relay reads are the same code.

| Module | Role |
| --- | --- |
| `verify` | cohort verification: signatures, bindings, receipts, published index, recomputed outcome, closure |
| `publish` | one run: discovery, copy, re-verification, artifacts, records, retention |
| `status` | typed status parsing and the public snapshot built from it |
| `alerts` | alert rules, sink payloads and delivery; the hold/renotify state machine and the post and heartbeat contract are the `slop-ninja-alerts-wire` crate (`../alerts-relay/wire`), shared with the relay |
| `index` | cohort index, publication records, reference pointers |
| `leaderboard` | leaderboard document and the static page |
| `redact` | what may leave the private side |
| `arena` | the bounded-memory rules and their measured constants |
| `layout` | every key read or written |
| `config` | vars and secrets; the compiled trust roots |
| `store` | the object-store seam, with an in-memory store for tests |
| `dashboard` | the public dashboard's `dashboard/v1` documents, their allow-list and bounds (the key grammar and bounds are `../dashboard-feed/src/contract.rs`, shared with the feed Worker) |
| `dashboard_roster` | (inside `dashboard`) the registry (`registry.json`) and per-cohort roster (`m/a/rosters/<label>.json`) documents behind the public status API's miner list, their checks, and the `DASHBOARD_ROSTER` levels for a running cohort |
| `cf` | Workers entry points (`wasm32` only): cron, R2 adapter, status reads, the `PublisherState` Durable Object |
| `durable` | the Durable Object state and its state-call bodies: JSON both ways (parsed with `serde_json`, never the runtime's `Request::json`), `u64` map keys read from a number or its decimal string |

Everything but `cf` is plain Rust and is tested natively over an in-memory
store.

## Safety properties

- **Allow-listed output only.** No cohort record is copied. The summary is
  built from typed fields of the verified records, checked against the
  allow-list (`allowlist::check_object`: its key, its schema and every field
  path) before it is written, and read back and checked again; a test checks
  every object a run writes, and that no exact score of the package (an
  entrant's, `R_k`'s or `R_k-1`'s, a probe or near-copy statistic) appears
  anywhere in the public bucket beyond the digits of a published `q`. Only
  the quantized score `q` (`quant-v1`) is published, under the summary's
  quantization block.
- **No public order of entrants.** An ordering is a channel an entrant with
  several hotkeys chooses (up to `log2(k!)` bits a cohort for `k` hotkeys),
  so the summary and the leaderboard carry no rank, list entrants in roster
  order and name a cluster by its first member in that order, never by its
  head; allow-list v3 refuses a `rank`, `rank_in_band` or cluster head
  field. Rewards (`reward-v2`) are computed from the published `q` values
  alone, with ties in `q` sharing credit and leftover units going in roster
  order, so the coordinator's signed row and the weights revealed on chain
  carry no order beyond the q-rank the published values already imply.
  The promoted entrant is the first head in q-rank whose exact score meets
  the margin and that passes promotion. Artifacts are published at
  scoring start, before any score exists, so their withholding never
  names a head or a cluster.
- **Only named objects.** The publisher reads only the cohort objects it
  verifies (never the receipts), and the artifact objects the result
  names. The batch shards, batch index, results, summaries, probe vectors,
  plans, pools and `keys.json` are never read or copied. Binary records
  decode canonically and completely.
- **Typed snapshot.** The status snapshot is built from named fields only.
  Spend, the ledger, generation, models, lanes, jobs, documents and fault
  details never reach it; faults are one boolean. The ledger enters the alert
  rules only as its `over_threshold` flag. `next` comes from the coordinator's
  calendar (the template's next cohort, its earliest opening epoch and its
  admission window in tempos; a
  queued cohort for coordinators without one), and `admitting` is set only
  while the calendar reports the cohort's admission window open.
- **No work-phase names.** `phase` is `admission` or `evaluating` for the
  active cohort (`queued`, `idle` or `unknown` without one). Admission opens
  at a tempo boundary and ends at the roster freeze, and a reward cohort
  closes at a tempo boundary; the phases in between (`generating`,
  `sealing`, `scoring`, `packaging`, `transition`) move when their work
  commits, so the snapshot, rewritten every minute, names none of them
  (`status::public_phase`; an unknown phase is `evaluating` too). The alert
  rules still read the coordinator's phase. This hides the phase name only;
  other public signals still move when hidden work commits. A probe build
  closes from packaging, mid-tempo, so its `evaluating` ends then. A
  cohort's records are published, and `last_published_cohort` moves, within
  a run of its package becoming durable (the start of `transition`), not at
  a tempo boundary. `fault` turns on when a step fails.
- **Artifacts are published at scoring start.** The user's copy model
  (2026-11-04): models become public when scoring starts. With the tracked
  `ARTIFACTS_FROM_EPOCH` set, each run looks at every in-record cohort that
  opened at or after it (and at or after `PUBLISH_FROM_EPOCH`, which it never
  moves) whose signed manifest exists (the roster is sealed) and that is not yet
  decided, and for each roster entry still pending:
  - withholds for good an operator or synthetic hotkey (`OPERATOR_HOTKEYS`);
  - reads the header object at the commitment's derived key
    (`admission/v2/c<cohort>/<commitment>/h`): R2's stored SHA-256 is the
    header digest (a missing checksum is refused and alerted, never a pass);
    the bytes, at most `MAX_ARTIFACT_HEADER_LEN` and reserved before the read,
    are decoded once their declared file count is within the artifact spec's
    table;
  - withholds for good a header or content digest in `PRIVATE_DIGESTS`, or an
    initial reference (`R_0` or its single replacement) any manifest named
    (the index keeps them in `initial_references`; references are published
    as reference packages instead);
  - compares each listed file's stored size and SHA-256 with the header (no
    byte read), then copies the files and last the header, so a public header
    names public files. `ARTIFACT_COPY = "stream"` (the tracked default until
    probe G7-x passes and `COPY_INTERFACE_TOKEN` is installed) pipes a binding
    `get` into a binding `put` carrying the digest, which R2 verifies.
    `ARTIFACT_COPY = "server"` (configuration, its own deploy) asks the
    internal signer Worker for a presigned `CopyObject` (source etag pinned,
    `x-amz-metadata-directive: REPLACE`, storage class `STANDARD`) and sends
    it, with no byte through this Worker; R2 answering 403 or 501 is a
    refusal, alerted. R2 answers a `CopyObject` only once the copy is done
    and abandons it when the caller disconnects (probe G7-x, X11), so the
    wait is derived from the object's length (`src/copy_wait.rs`,
    `slop_ninja_r2_copy_wait_v1`): the outbound request bound plus twice the
    time at the slowest measured cross-bucket rate (268,435,456 random bytes
    in 25.4 s, X12). It must end inside the step's two-minute lease, less one
    request bound before (the signer) and one after (the `head()`), which
    leaves 100 s; an object above the length that fits (475,574,626 B) is
    copied by stream instead, without asking the signer. At its wait the copy
    request is aborted (`AbortSignal.timeout`), which closes the connection,
    so R2 abandons the copy. A copy that is not confirmed (aborted, failed in
    transport, or answered an error other than 403, 501 or the stale source's
    412) is never retried forever: the destination is read first, and an
    absent one is copied by stream in the same step under a renewed lease;
    since the `CopyObject` may still land over that stream copy, the result
    then passes by either its stored SHA-256 or the source's etag. Each
    public copy is
    then checked by `head()`: its size, and, for a `CopyObject` (which keeps
    the etag but never carries the stored SHA-256, qualification G7), its
    etag against the admitted source's; for a stream copy, its stored
    SHA-256 (`files[].check` records which, so a `server` record can list
    both). A public object a run finds already there passes by either.

  Nothing hashes a byte on this path; `subnet/tests/no_object_hash.rs` scans
  it. The scores do not exist at scoring start, so the public-copy gate and
  the near-copy clusters no longer decide publication. Uploading through the
  admission route accepts the CC BY-SA 4.0 submission terms; the roster's
  signed commitments and the derived keys (which only the coordinator's
  presigned URLs can fill, with R2-verified bytes) stand for the upload
  envelope the closure-time copy used to read. The cohort's record
  `v1/cohorts/<label>/artifacts.json` lists each entry's state, header
  digest and published keys. Entries wait (an upload not there yet, a store
  error, the wall budget) and are retried on every run, never skipped or
  ended by time; a decided cohort is not read again. When the cohort closes,
  `publication.json` takes each entrant's state from that record: a published
  header counts only when it is the result's artifact digest (alerted
  otherwise, `publication_refused`), the static checks still withhold what
  was not published, and a published object is never recalled. `R_k` is
  downloadable when it is a promoted miner artifact published here.
  `ARTIFACT_SOURCE_KEY` and `ADMISSION_RECORD_KEY` (closure-time copies from
  the eval bucket) are retired: a deployment that still sets one gets a
  `config_warning`, as does the retired `PUBLISH_ARTIFACTS`. A set
  `ARTIFACTS_FROM_EPOCH` without a non-empty `PRIVATE_DIGESTS` and
  `OPERATOR_HOTKEYS` is refused (`config_refused`): the boundary is tracked,
  so a routine deploy that omits the deploy-time guards is refused and
  alerted, never a silent stop of publication (which would also keep the
  admitted objects in the admission bucket, since the eval Worker deletes
  them only once a closed cohort's publication is reported).
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
  refused. The index records the prefix it was built under
  (`record_prefix`). When the configured prefix is one a listed cohort's
  label lacks (a chain-record restart under a new main authority), the run
  copies the previous record's index to
  `v1/records/<its first label>/cohorts/index.json`, removes the live
  leaderboard and reference pointers, and starts a fresh index, so a
  restarted record never appends to the earlier one; the earlier cohorts'
  directories stay where they are. Only the index is archived: it is built
  from typed fields, while the leaderboard and pointers carry the wording of
  the build that wrote them, which an archive would keep public for good. The
  archive is scanned for wording public text never carries before it is
  written (`redact::scan_wording`; a hit refuses the run). An archive object
  is written once: one that already holds other bytes refuses the run, so an
  earlier archive is never replaced. The fresh index records each earlier
  record (`previous_records`: its prefix and first label, oldest first, and
  its initial references, whose artifacts stay withheld as entrant artifacts
  in every later record, whatever the publisher's memo holds). A configured
  prefix that names an earlier record (its recorded
  prefix, or one its first label has; a deploy from a tree that still tracks
  it) is refused: the run moves, adopts and publishes nothing and reports
  why. The archived index is the record as it stood. Refusals of the
  earlier record's cohorts stay out of the live record's `refused` list.
  A refused cohort is not read again by the same
  build (version and trust roots); a new build retries it.
- **Read-only on the private side.** R2 bindings cannot be made read-only,
  so the code writes and deletes only in the public-candidate bucket (the
  receipts and admission buckets are only read). The tests check that the
  private buckets see no write or delete.

## Bounded memory

`arena.rs` holds the rules (`slop_ninja.publisher.arena/v2`). Runs no longer
read receipts (they are coordinator-private), so the receipt-index and bundle
bounds below apply to the verifier's receipt path (`verify::Options::receipts`),
which the measurements exercise and the run does not take.

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
- Copies, artifacts included, stream inside the runtime (or run inside R2,
  `ARTIFACT_COPY = "server"`) and never enter Wasm memory. An artifact header
  is read within `MAX_ARTIFACT_HEADER_LEN`, with its decoded file table (the
  spec's file count at most, each name at its longest) reserved first.

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

The rule set is `slop_ninja.publisher.alerts/v16` (v16 adds
`commit_stalled` and `signing_backing_off`; v15 adds
`row_record_stalled`; v14 adds `operator_clear_needed`, names a fault's
cause and any jobs at the receipt bound in the fault rules' detail, and
drops the retired `monthly_cap` code from the scorer and budget faults; v12
and v13 are `status_access_warning` and the public receipts cross-check).
The table is in `alerts.rs`:

- durable faults;
- fallback lasting a tempo;
- a webhook subscription presumed paused (`webhook_paused`: the Worker's
  `chain_journal.watchdog.subscription.state` is `presumed_paused`, so no
  delivery has arrived for longer than the provider can retry one while
  blocks are owed: the subscription is paused, which the provider keeps
  until an operator resumes it, or the provider has a stall on its own side,
  which never pauses; the operator checks the subscription's state before
  resuming, and fallback carries the cursor meanwhile);
- a stalled confirmed watermark;
- a work phase past its package deadline, or (v8) a cohort still in
  `admission` there with its window closed (its roster freeze waiting; the detail names
  on what, from the calendar's `freeze_wait`);
- scorer, budget and other cohort faults, including a root step paused as
  `step_refused` and (v7) a closed cohort whose closure step is paused, and
  (v10, `cohort_fault`) a root step of such a cohort that keeps failing on a
  retried code (`step_error`) through a whole tempo of chain progress: the
  eval status's stored epoch two past `step_error.since_epoch`, which the
  coordinator keeps from the first failure until a step succeeds, whatever
  code later failures carry (v11), so alternating transient codes alert
  too; nothing is paused by it; (v14) the detail names the fault's cause
  (`fault_cause`) and any jobs at the receipt bound
  (`receipt_bound_reached_jobs`), and a fault the coordinator reports as
  not clearing by itself (`operator_clear_needed`: not a probed outage in
  a work phase nor an admission outage it re-reads) also raises
  `operator_clear_needed`, so a pause that waits for an operator is told
  apart from one a probe resolves;
- (v11) alert-only cohort conditions, none of which pauses anything: a
  cohort past a budget guard (`scorer_budget_fault` from `budget_alert`;
  spend is tracked, never capped), and a cohort whose chunks got
  infrastructure attempts past `max_attempts` (`attempts_extended` from
  `attempt_extensions`);
- (v11) the eval status's stored epoch held back by unknown head epochs
  (`epoch_deferred`: `watermark_epoch`, the epoch index at the confirmed
  watermark, two or more past `observed.epoch.index`, a whole tempo of
  watermark progress judged by chain blocks); the cohort schedule is not
  advancing meanwhile;
- (v11) a coordinator commit operation that settled with its inclusion
  unknown (`inclusion_unknown`, subject `coordinator.epoch-N`: detail
  `inclusion_unverifiable`; one alert per operation while it is among the
  recent operations; the signer keeps running);
- the spend ledger's billing month past the deployment's private alert
  threshold (the ledger's `over_threshold` flag; tracking only);
- receipt deletion falling behind the N+2 rule (`retention_stalled`: the
  eval status's `receipt_retention.stalled` lists a due cohort whose
  detailed receipts remain after a further completion, or a predecessor
  authority not finished after one; judged by completions, never a clock;
  the detail names the last failed step's code, such as an unreadable
  authorization pointer); and (v13, subject `public_receipts`) the
  publisher's own N+2 reading disagreeing with the coordinator's markers: a
  marker whose due point the public record has reached while the public
  reading still holds the cohort, or a cohort the public reading found due
  with no marker after two further completions (claimed only in a run that
  read the authorization pointer). This build publishes no detailed
  receipt, so it makes no such reading: every finished run reports none,
  which resolves an alert a v13 build raised;
- two tempos without a coordinator commit (`commit_gap`: the confirmed
  epoch two past the last committed one, so one missed epoch, whether or
  not the coordinator is authorized or paused);
- (v16) a stalled signer while the coordinator is authorized and active
  (`commit_stalled`): two whole epochs without a weight commit, judged on
  epoch boundaries. An epoch's commit window opens the commit offset
  (`SIGN_COMMIT_OFFSET`) after its boundary and closes before the next
  boundary, so the current epoch never counts and two missed epochs means
  the confirmed epoch is three past the last committed one (or past the
  authorization's issue epoch, when that is later). The detail carries the
  signer's health, backoff and last error code;
- (v16) a signer that keeps failing without progress for a tempo
  (`signing_backing_off`): `signing.last_error_age_ms` younger than the
  newest `signing.log` row's age, held for a whole tempo of observation.
  Health reads `ok` between retries (`backing_off` lasts only while the
  backoff is running), so the error's recency against the log, which only
  progress moves, is what the rule reads. A commit-to-reveal gap is under a
  tempo, so one transient error does not fire it. On 2026-11-05/06 the
  signer retried an unreadable inclusion block for 14 hours from epoch 137
  with health `backing_off`; only `commit_gap` fired;
- the coordinator's authorization-pointer writer not confirming the private
  pointer for a tempo (`pointer_stale`: `signing.authorization` reports the
  last pass failed, `pointer_error`, or a last confirmation older than two
  cron periods; re-authorizations and revocations reach validators only
  through that pointer);
- the coordinator's automatic target handoff reporting the same failure
  for a tempo (`auto_target_stalled`: `signing.auto_target.error`, with the
  cohort label it is stuck on, for example a refused bundle,
  `receipts_retired`, `receipts_missing`, `earlier_cohort_unclosed`,
  `cohort_gap`, `cohort_behind_record`, `manifest_undecodable`,
  `pass_budget` or
  `record_authorities_unset`; the cohorts after it wait, none is skipped;
  v9), or its authorization key's check failing for a
  tempo (`signing.auto_target.key_error`, reported before any cohort needs
  a signature);
- no signed coordinator row covering the installed authorization for a
  tempo, or signing a new row failing for a tempo while the old one still
  covers it (`row_record_stalled`, v15: `signing.row_record.pending_*` or
  `error`, with the code it waits on, such as `row_waiting_derivation`,
  `row_waiting_issue`, `row_hotkeys_missing` or a key failure); validators
  keep their installed row meanwhile, and a fresh validator waits;
- a coordinator reveal that differs from its committed row
  (`coordinator_reveal_mismatch`);
- unreachable status, and a status that answered without a field an open
  rule reads (`status_incomplete`);
- refused or stalled publications: cohorts, artifact admission records and
  copies, and the authorization;
- unverifiable manifests (one aggregated alert), and revocation-set entries
  that do not open under the trust roots (`revocation_unverifiable`);
- a refused configuration, and a setting the configuration ignored
  (`config_warning`);
- (v12) a Worker's status gate reporting two of its status tokens equal, at
  once, or the shared pre-split status token still installed beside both
  consumer tokens for a tempo (`status_access_warning`, subject the Worker;
  read from `/status` `access`, which never reaches the public snapshot);
- runs interrupted for a whole tempo.

Each condition has a hold time. It fires once the hold passes, is re-posted
every `ALERT_RENOTIFY_SECS` while it lasts, and posts a resolution when it
clears. A failed post is retried by the next run. A duplicate run posts
nothing new. Posts are content-free.

**Unknown is never a resolution** (v4). A status read that is unanswered,
times out or fails, or answers without a field a rule reads, decides
nothing: the open entries of the rules that read depends on
(`durable_fault` on its Worker's journal block, and a `.signing` subject on
its signing view; `fallback_too_long` on its Worker's mode, and the mode's
age while in fallback; `webhook_paused` on its Worker's subscription
state; `status_access_warning` on its Worker's `access` report;
`watermark_stalled` on its Worker's answer; the cohort
rules on the eval status's complete cohort list (every cohort with its
number, label and phase); `phase_overrun` also on its epoch and, while its
cohort is the active one in a work phase or in `admission`, that cohort's
`opened_epoch` and `k_tempos`; `ledger_threshold` on its ledger flag,
`ledger_threshold_unset` on its ledger standing and `retention_stalled` on
its `receipt_retention.stalled` list; `commit_gap` and
`coordinator_reveal_mismatch` on the coordinator's signing view and the
eval epoch) are carried, neither fired nor resolved. A carried entry keeps
its last decided detail with a note of how long its read has been unknown
and why (`... [unknown for 3600 s: coordinator status did not answer]`), so
a reminder sent meanwhile never reads as a fresh observation; the note does
not change event ids. A
condition resolves only on a read that answered and shows it clear, and an
unknown read never raises a new one. The unknown read itself alerts after
one hold (two cron periods): a source that does not answer is
`status_unreachable`; a source that answers without a field a carried entry
needs is `status_incomplete <source>`, whose detail names the fields and the
entries they leave undecided (for example a status shape that lost the
coordinator's signing view during a standing `commit_gap`). One missed read
sends nothing, and a longer outage sends one alert and its resolution. An
entry that a successful read already cleared stays resolving. Limit: a
missing field with no open entry raises nothing; the rule that reads it
cannot fire until the field is back.
The tests replay the 2026-11-02 18:52Z case (one timed-out coordinator read
during a standing `commit_gap`) and a script of mixed answered and unknown
reads, and show one message per real transition. Events are split into posts within the
sink's limit (2,000 characters for Discord, 40,000 for Slack and for the JSON
body), and each delivered post is acknowledged on its own.

Every event carries an id derived from the alert state (the rule set, kind,
rule, subject, when the condition was first seen and when it was last
notified), and the last-notified time it is derived from. A retried post
repeats its ids, and the next reminder has new ones. A firing is marked as
attempted before its post. If its answer is lost and the condition clears
before a retry is acknowledged, a resolution with no last-notified time is
posted. The relay delivers it only if the firing went out; a URL sink
receives it as it is. A sink that does not
answer at all is not asked again in the same run, so a hung sink costs one
outbound bound per run.

The post schema is `slop_ninja.publisher.alert/v3`. **Breaking change for
generic JSON receivers:** the deployed publisher's JSON sink sent
`slop_ninja.publisher.alert/v1`, whose events had no `id` or `notified_ms`.
A receiver that validates v1, or refuses unknown fields, must be updated
before this build is deployed with `ALERT_SINK_FORMAT = "json"`. The Slack
and Discord formats are unchanged.

One sink at a time:

- **The alerts relay** (decision D18, `ALERT_RELAY = "true"`): the JSON body,
  posted through the `ALERTS_RELAY_TESTNET` service binding. The relay writes
  each event id once to its alert feed (and emails it once while its email
  is on) and answers 2xx only when every event of the post is settled, so a
  post whose answer was lost is retried and delivers nothing twice. A JSON event longer than the limit has its detail shortened (rule
  and subject kept), so every post fits the relay's body bound.
- **An https URL** (`ALERT_SINK_URL`), shaped by `ALERT_SINK_FORMAT` for a
  generic JSON receiver, a Slack-style `{"text"}` webhook or a Discord-style
  `{"content"}` webhook. Such a sink keeps no record of what it posted, so
  the publisher never sends it a resolution of a firing whose post was not
  acknowledged: that entry is dropped when it clears, as before the relay
  sink existed.
- With both set, the relay wins. The URL is ignored and raised as
  `config_warning` ("remove the secret"). This is never a refusal, because
  the configuration also drives publication, and validators depend on
  publication.

`ALERT_RENOTIFY_SECS` must be at least one cron period (60 s). A reminder
cannot come more often than the runs do, so a shorter value, including 0,
is refused rather than read as "every run".

A run first writes the snapshot and evaluates the alerts, using the
publication results of the previous run, then saves its state, and only
then starts cohort work. When alerts are due, it also saves the observed
alert state, with the marks of the posts about to go out, before it posts
any of them, and posts nothing when that save fails. The event ids derive
from that state, so a run that dies after posting, or whose later save
fails, leaves the next run the same ids, and the relay delivers them once.
A resolution whose post was attempted marks its entry: if the condition
comes back before the resolution is acknowledged, it fires again as a new
episode instead of waiting for the next reminder. Each step (a cohort verification, an artifact
copy) renews a two-minute lease (`PUBLISHER_LEASE_MS` of the alerts wire
crate) and starts only within `RUN_WALL_BUDGET_MS` (default one cron
period). A run killed mid-step blocks the next runs for
one lease at most, and a blocked run still refreshes the snapshot. A refused
configuration still writes a fault snapshot and raises `config_refused`
through whatever sink parses.

The dead-man check is the relay's (decision D18). With `ALERT_RELAY = "true"`,
each run posts a heartbeat (`slop_ninja.publisher.heartbeat/v3`) to the relay
once its alert phase has saved its state:

- A run blocked by a held lease, or whose state object did not answer,
  sends none. A run whose configuration was refused sends one after it
  alerted the refusal.
- The heartbeat goes out beside phase 2 (kept alive by `wait_until`), so a
  slow relay does not take phase 2's wall budget.
- It carries the post schema this build sends, how many due events the run
  left undelivered and since which run, and this configuration's
  `watermark_stalled` bound, which the relay uses for the coordinator while
  the publisher is down. The relay alerts when posts keep failing while
  heartbeats arrive.
- The relay is a separate Worker with its own cron, so a crashed publisher,
  a stopped cron or a broken binding is caught.
- Its bounds come from the cron period, lease, hold and status-read bound
  that this crate takes from the shared alerts wire crate (see the relay's
  README).

An external check can still be pinged: `HEARTBEAT_URL`, enabled only by
`HEARTBEAT_ENABLED = "true"`, is a GET after each run that finished both
phases.

## Dashboard documents

After the publication pass, each run writes the public dashboard's
documents (`index.json`, `m/a/now.json`, `m/a/cohorts/index.json`, one
document per listed or ended cohort, and from bounds rule v3 the registry
`registry.json` and the per-cohort rosters `m/a/rosters/<label>.json`)
under `dashboard/v1/` into the
dashboard bucket (binding `DASHBOARD_TESTNET`). That bucket holds nothing
else, and only the private dashboard feed Worker (`../dashboard-feed`) reads
it. The website reaches the feed through a service binding. Without the
binding, no dashboard document is written. The contract, its allow-list,
bounds and inputs are in
[`docs/testnet/dashboard-contract.md`](../../docs/testnet/dashboard-contract.md).

Every write passes `dashboard::check`: the key grammar, the kind's byte
bound, the schema, the kind's path allow-list, the redaction scan,
`sample: false`, and a work percent only on the 10% grid for the current
work stage. The stage and its phases are shown as of the last observed
tempo boundary. The work percent and a cohort fault's pause class are read
by the first observation of a boundary that follows the saved one in
sequence, saved to the Durable Object state before any document carries
them (nothing is written when that save fails: the run keeps the saved
memo, and the refusal, carried by the run's final save, raises
`publication_refused` and counts in the run log's refused total), and kept until the next
boundary; a fresh or lost state, or a jump past several boundaries, leaves
them out for that tempo. While the coordinator's status is unreadable, only
`index.json` and `now.json` are written. Cohort documents are rebuilt only
when their inputs change; a rebuild the wall budget cuts short resumes after
its last document while its inputs stay the same. Their summary and
publication reads are reserved from the run's memory budget. A refused write
is alerted like a refused publication.

The registry is written each run from a coordinator whose `/status` reports
`chain_journal.registry` (journal schema v14 on). A closed, listed
cohort's roster is rebuilt with its cohort document from the same summary
and publication record; a running cohort's roster is written each run at
the `DASHBOARD_ROSTER` level (tracked in `wrangler.toml`: `sessions` on
testnet, the approved public intake state; `closed`, none, when
unset), from the eval Worker's `admission.cohorts[].hotkeys` or, from
scoring start without them, the published artifacts record. At `sessions`
each admitted entrant's row also carries its challenge counts
(`challenges{state, done, total}`, from `admission.cohorts[].progress`,
rule `slop_ninja.eval.scoring_progress/v1`, read each run): counts only,
under the decision of 2026-11-06. At `live` (a reviewed change of the
tracked value, after a website that accepts the level) a running roster
also carries the cohort's live values from the eval Worker's `/eval/live`
(rule `slop_ninja.eval.live_scores/v1`, read only at that level and parsed
whole or not at all, with the grid, noise and bits recomputed here):
`live-v1` segment readings and pre-finalized quantized scores. A roster
says `live` only when it carries them; a stop, a refused report or a
roster that fails the dashboard check is written at `sessions` with its
counts (`live_dropped`), a value that would replace one already published
is never written (`live_value_changed`, against a durable record in the
state object), and every pre-finalized value shown is compared with the
result at close (`prefinal_mismatch`). No level below `live` publishes a
score, and no level publishes a rank, roster position, gate, band or
cluster before close. The fields, levels and inputs are in
[`docs/testnet/public-layout.md`](../../docs/testnet/public-layout.md#its-dashboard-documents-registryjson-and-rosterslabeljson).

Every stage carries a start (its phase's boundary point: epoch, boundary
block, time to the minute and how the time is known) and stops where the next
phase starts, with its tempos (elapsed, credited, required). The points come
from the eval engine's resolved boundaries (`/eval/status`
`cohorts[].boundaries`, from its boundary journal), so stages before this
publisher ran are dated too; a boundary the engine has no record of reads as
not recorded, and one it has records around but cannot date as
`unresolved`. Every block agrees with a reference: the clock's boundary in
`now.json`, and in the cohort list and documents each row's `anchor` (the
clock's boundary projected to the cohort's opening at the clock's tempo);
without one no block is written. `dashboard::check` also refuses a
`now.json` stage dated after the clock's boundary and a cohort document stage
after the close. A coordinator that reports no boundaries falls back to the
boundary-step records and the minutes this publisher observed boundaries in
sequence. Before writing, the builders drop any point that fails the
contract's checks (a block off the boundary arithmetic, a time off the
minute, after the document or going backwards, a basis outside the contract,
wrong tempos), and `dashboard::check` refuses a document those checks would
still change. Rule `slop_ninja.dashboard.publisher/v2` rebuilds every cohort
document once.

## Configuration

See `wrangler.toml`. `PUBLISH_FROM_EPOCH` and `RECORD_LABEL_PREFIX` are
tracked there (never passed with `--var`, so every deploy carries the record
its tree names; a restart changes both in one commit). Before every deploy,
`cargo run --example slop_ninja_publisher_ops -- record-check --wrangler
wrangler.toml --index <the live v1/cohorts/index.json>` refuses (exit 1) a
tree whose `RECORD_LABEL_PREFIX` is not the live record's, an earlier
record's above all (`index::record_check`); `--restart` passes only a new
prefix, for a restart's commit. The vars set at deploy
are `PRIVATE_DIGESTS`, `OPERATOR_HOTKEYS` and `NOTICE` (every publisher
deploy passes the first two once `ARTIFACTS_FROM_EPOCH` is set; without
them the configuration is refused). `ARTIFACTS_FROM_EPOCH` is tracked. `ARTIFACT_COPY` (`"stream"` until probe G7-x passes; `"server"`
after it, as its own deploy) and `ADMISSION_BUCKET_NAME` are
tracked. The server-side copy also needs the `R2_SIGNER_TESTNET` service
binding (tracked; deploy the signer first) and the `COPY_INTERFACE_TOKEN`
secret, shared with the signer; without the token every copy waits with a
pending reason. `ARTIFACT_COPY = "stream"` needs neither. `ADMISSION_ORIGIN` is tracked (`https://testnet-admission.slop.ninja`,
the origin `testnet-trust.json` pins). `ALERT_RELAY` is tracked (`"true"`, decision D18). The secrets are
`COORDINATOR_STATUS_TOKEN` (the publisher's status token for the coordinator
Worker: that Worker's `STATUS_TOKEN_PUBLISHER`, or the shared pre-split token
until the split; coordinator README, "The status gate"),
`EVAL_STATUS_TOKEN` (the same for the eval Worker, a different value), and the
optional `ALERT_SINK_URL` (ignored, with a warning, while the relay is on)
and `HEARTBEAT_URL`. Before a deploy that turns the relay on, check that
`wrangler secret list --env testnet` lists no `ALERT_SINK_URL`. The trust roots are
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
and package. A revocation must decode and name this chain and is copied
whatever target it names (validators journal those that verify before
anything else); one that does not open under this build's trust roots is
copied unchanged and alerted, so a trust-root rotation deployed to the
coordinator first never stops newer authorizations from being published.
The objects are then
copied to `v1/authorizations/` and the public pointer is written. The
coordinator rewrites the private pointer whenever it differs from its
installed bundle and journaled revocations; nothing in it expires.

## Checks

On the project's build host, as a memory-limited job under the shared cargo
lock:

```sh
cargo test --manifest-path subnet/publisher/Cargo.toml
cargo check --manifest-path subnet/publisher/Cargo.toml --target wasm32-unknown-unknown
```

## Not yet done

- Deploy rule set v4 (the unknown-read rule) after review. Its first run
  keeps every open entry and its ids change scope (the rule set names
  them): already notified entries post nothing new, and a firing whose post
  was never acknowledged before the deploy resolves without a message (the
  relay looks its firing up under v4).
- Live checks:
  - anonymous GETs;
  - drills of the remaining alert rules through the relay (done:
    `config_warning`, and the relay's `publisher_silent` heartbeat miss);
  - a retried post after a lost answer (one delivery; covered by the relay's
    native tests only);
  - a server-side `CopyObject` from the admission bucket to the public
    bucket (qualified within one bucket on 2026-11-05: the etag is kept, the
    stored SHA-256 is not carried, `REPLACE` drops stored headers; the
    cross-bucket copy awaits the deployed copy credential);
  - a stream copy of a full-size artifact file (the tower graph,
    681,508,964 B) between buckets inside one run's wall budget, for the
    fallback.

  Whether R2 accepts an R2 body stream as a put body without a
  `FixedLengthStream` wrapper is the first thing the live copy confirms.
- Coordinator follow-ups (its paths belong to the reward-v1 live stream until
  that branch lands):
  - keep admitted objects in the admission bucket until publication at
    scoring start has decided their entrant (admission v2's retention);
  - refuse plans whose receipt count exceeds the publisher's bound above;
  - a compact status route for the publisher (the active and next cohort,
    the regime, the ledger flag and a journal summary). `/eval/status` lists
    every cohort ever held, so its size grows with the record; today it is
    about 6 KB a cohort, read within the arena budget;
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

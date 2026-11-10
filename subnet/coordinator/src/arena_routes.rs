//! The coordinator's arena routes: identities, abandonment deadlines and
//! control footprints for every Worker and Durable Object entry point, the
//! shape-derived footprints of the large cohort steps, and the isolate's home
//! scope for the two bounded in-memory caches.
//!
//! Compiled natively and for wasm32: the Workers entry points (`cf_arena`)
//! and the native witness tests use the same constants, so a footprint the
//! tests check is the one the Worker admits.
//!
//! Every entry point opens one scope (`slop_ninja_arena::enter`) whose heap
//! holds what the handler allocates, and closes it when the handler ends: the
//! per-invocation reset. A footprint is admitted before the handler runs, so
//! a pool that cannot hold it refuses the invocation cleanly (503, a re-armed
//! alarm, a retried batch, a skipped cron tick, or a durable cohort pause)
//! instead of failing an allocation half way. Allocation beyond the admitted
//! footprint is an overdraft: served from free pages and counted on `/status`
//! in `Enforce`, refused in the strict witness tests.

use slop_ninja_arena::{AsyncRoute, Route, limits};

/// Route ids index the arena's fixed per-route counters (`u8` key space).
pub mod id {
    pub const HOOK: u8 = 1;
    pub const HOOK_CONTINUATION: u8 = 2;
    pub const STATUS: u8 = 3;
    pub const OPERATOR: u8 = 4;
    pub const EVAL: u8 = 5;
    pub const OTHER: u8 = 6;
    pub const SCHEDULED: u8 = 7;
    pub const QUEUE: u8 = 8;
    pub const ADMISSION: u8 = 9;
    pub const JOURNAL_NEW: u8 = 10;
    pub const JOURNAL_FETCH: u8 = 11;
    pub const JOURNAL_AUTO: u8 = 12;
    pub const QUEUE_ADMISSION: u8 = 13;
    pub const REFERENCE: u8 = 14;
    pub const COHORT_NEW: u8 = 20;
    pub const COHORT_FETCH: u8 = 21;
    pub const COHORT_ALARM: u8 = 22;
    pub const COHORT_SEAL: u8 = 23;
    pub const COHORT_DRAW: u8 = 24;
    pub const COHORT_PACKAGE: u8 = 25;
    pub const COHORT_CLOSE: u8 = 26;
    pub const COHORT_LOAD_SEAL: u8 = 27;
    pub const COHORT_COMMITMENTS: u8 = 28;
    pub const COHORT_ADMISSION: u8 = 29;
    pub const HOME_STATE: u8 = 30;
    pub const HOME_RUNTIMES: u8 = 31;
    pub const HOME_PLANS: u8 = 32;
    pub const COHORT_WORK: u8 = 33;
    pub const COHORT_LIVE: u8 = 34;
}

/// The deadline of an entry with no documented wall limit (Worker and
/// Durable Object fetch: "Unlimited"). Their awaits are not all bounded by
/// their own timeouts yet, so the bound is the longest documented wall limit
/// of any entry kind (cron, queue and alarm handlers: 15 minutes); a scope
/// unpolled for longer is abandoned, and a late poll still recovers it.
pub const UNBOUNDED_FETCH_MS: u64 = limits::ALARM_MS;

/// The webhook delivery: every await is bounded, so its deadline is their sum
/// plus the CPU limit: the body read (`webhook::DELIVERY_ACK_MS`, raced in
/// JS) and the journal's `/ingest` call with its reply
/// (`liveness::TIMEOUT_MS`, an `AbortSignal` timeout).
pub const R_HOOK: AsyncRoute = AsyncRoute::new(
    id::HOOK,
    "worker/hook",
    crate::webhook::DELIVERY_ACK_MS + crate::liveness::TIMEOUT_MS + limits::REQUEST_CPU_MS,
);
/// The delivery's `waitUntil` continuation: the documented 30 s limit.
pub const R_HOOK_CONTINUATION: AsyncRoute =
    AsyncRoute::new(id::HOOK_CONTINUATION, "worker/hook-continuation", limits::WAIT_UNTIL_MS);
pub const R_STATUS: AsyncRoute = AsyncRoute::new(id::STATUS, "worker/status", UNBOUNDED_FETCH_MS);
pub const R_OPERATOR: AsyncRoute = AsyncRoute::new(id::OPERATOR, "worker/operator", UNBOUNDED_FETCH_MS);
pub const R_EVAL: AsyncRoute = AsyncRoute::new(id::EVAL, "worker/eval", UNBOUNDED_FETCH_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "worker/other", UNBOUNDED_FETCH_MS);
pub const R_SCHEDULED: AsyncRoute = AsyncRoute::new(id::SCHEDULED, "worker/scheduled", limits::CRON_MS);
pub const R_QUEUE: AsyncRoute = AsyncRoute::new(id::QUEUE, "worker/queue", limits::QUEUE_MS);
/// One R2 object-create event delivery of the admission events Queue (a
/// wakeup: the cohort object heads the object itself), a child of the
/// batch's scope (`ADMISSION_EVENT_BYTES`).
pub const R_QUEUE_ADMISSION: Route = Route::new(id::QUEUE_ADMISSION, "worker/queue-admission-event");
/// The internal admission interface (the upload Worker's service binding
/// only; no public route): the route holds one forwarded message and the
/// cohort object's reply. No part body reaches this Worker.
pub const R_ADMISSION: AsyncRoute = AsyncRoute::new(id::ADMISSION, "worker/admission", UNBOUNDED_FETCH_MS);
/// Reference staging's operator routes (`/eval/reference/`,
/// [`REFERENCE_CONTROL_BYTES`]).
pub const R_REFERENCE: AsyncRoute = AsyncRoute::new(id::REFERENCE, "worker/reference", UNBOUNDED_FETCH_MS);
/// A Durable Object constructor's storage migration: synchronous.
pub const R_JOURNAL_NEW: Route = Route::new(id::JOURNAL_NEW, "journal/new");
pub const R_JOURNAL_FETCH: AsyncRoute = AsyncRoute::new(id::JOURNAL_FETCH, "journal/fetch", UNBOUNDED_FETCH_MS);
/// The ChainJournal's automatic-handoff pass (`/sign-auto`): its own route,
/// so `/status` reports its peak beside the other journal requests'.
pub const R_JOURNAL_AUTO: AsyncRoute = AsyncRoute::new(id::JOURNAL_AUTO, "journal/sign-auto", UNBOUNDED_FETCH_MS);
pub const R_COHORT_NEW: Route = Route::new(id::COHORT_NEW, "cohort/new");
pub const R_COHORT_FETCH: AsyncRoute = AsyncRoute::new(id::COHORT_FETCH, "cohort/fetch", UNBOUNDED_FETCH_MS);
pub const R_COHORT_ALARM: AsyncRoute = AsyncRoute::new(id::COHORT_ALARM, "cohort/alarm", limits::ALARM_MS);
/// The cohort steps: each a child of the alarm's scope (it takes the
/// alarm's deadline), one route per step kind so `/status` reports each
/// kind's peak pages. The seal and package steps hold their shape-derived
/// footprints (`seal_bytes`, `package_step_bytes`), admitted physically
/// (`ScopeRef::child_physical`); the draw, close and load-seal steps have no
/// derived footprint yet and open with `STEP_BASE_BYTES`, their use beyond
/// it counted as overdrafts of their own route.
pub const R_COHORT_SEAL: Route = Route::new(id::COHORT_SEAL, "cohort/seal");
pub const R_COHORT_DRAW: Route = Route::new(id::COHORT_DRAW, "cohort/draw");
pub const R_COHORT_PACKAGE: Route = Route::new(id::COHORT_PACKAGE, "cohort/package");
pub const R_COHORT_CLOSE: Route = Route::new(id::COHORT_CLOSE, "cohort/close");
pub const R_COHORT_LOAD_SEAL: Route = Route::new(id::COHORT_LOAD_SEAL, "cohort/load-seal");
/// The live values' share of a step (`eval::live`): one setup or one
/// release of one cohort per child scope ([`live_step_bytes`]).
pub const R_COHORT_LIVE: Route = Route::new(id::COHORT_LIVE, "cohort/live");
/// The commitment reader's rounds: the stored runtime's parse and one
/// round's replies (`COMMITMENTS_CONTROL_BYTES`).
pub const R_COHORT_COMMITMENTS: AsyncRoute = AsyncRoute::new(id::COHORT_COMMITMENTS, "cohort/commitments", UNBOUNDED_FETCH_MS);
/// The admission steps (the canary, the close sweep, manifest writes and
/// audits, verdict runs, the alarm sweep, deletes, the reconciliation, the
/// calendar), each a child of the alarm (`ADMISSION_STEP_BYTES`).
pub const R_COHORT_ADMISSION: Route = Route::new(id::COHORT_ADMISSION, "cohort/admission");
/// The cohort object's admission session operations (`/admission/*`: the
/// forwarded requests and the R2 events' `observe`), which await R2, the
/// signer and the upload Worker's header check ([`cohort_session_bytes`]).
pub const R_COHORT_SESSION: AsyncRoute = AsyncRoute::new(id::COHORT_WORK, "cohort/admission-session", UNBOUNDED_FETCH_MS);
/// The isolate's home scopes: synchronous, each opened on first use and kept
/// for the instance's life.
pub const R_HOME_STATE: Route = Route::new(id::HOME_STATE, "home/state");
pub const R_HOME_RUNTIMES: Route = Route::new(id::HOME_RUNTIMES, "home/runtimes");
pub const R_HOME_PLANS: Route = Route::new(id::HOME_PLANS, "home/plans");

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// Control footprints: what a route allocates besides the bodies it admits
/// as exact blocks once their length is known (`Content-Length`, an R2
/// object's size, an RPC reply's declared length). The witness tests
/// (`arena_witness.rs`) run the heaviest native path of the routes they
/// cover under `Strict` with exactly this footprint; the routes they do not
/// cover yet (the continuation and cron RPC rounds, the queue batch, the
/// Worker status aggregation) and the workers-rs request and response
/// conversion the native tests cannot reach are measured on the deployed
/// instances, whose per-route peaks `/status` reports (`Observe` mode, design
/// migration step 1).
/// The webhook delivery: its authenticated parse and the journal call (the
/// body itself, at most `webhook::MAX_BODY_BYTES`, is an admitted block).
pub const HOOK_CONTROL_BYTES: usize = 256 * KIB;
/// The delivery continuation and the cron tick: RPC rounds and signing
/// rounds, one call at a time (an RPC reply above the control footprint is
/// an admitted block of its declared length).
pub const CONTINUATION_CONTROL_BYTES: usize = 2 * MIB;
/// The Worker's status, operator, eval and fallback routes (an operator body
/// is an admitted block).
pub const WORKER_CONTROL_BYTES: usize = MIB;
/// The lane-wakeup queue batch: at most 100 content-free kicks.
pub const QUEUE_CONTROL_BYTES: usize = 512 * KIB;
/// A ChainJournal request: one transaction over the journal. Its base is
/// [`JOURNAL_BASE_BYTES`]; its heaviest path is a signing step reading a
/// stored runtime ([`journal_runtime_bytes`]) at the largest metadata the
/// journal can store ([`MAX_STORED_RUNTIME_METADATA_BYTES`]), so no stored
/// runtime overdraws it, whatever its spec.
pub const JOURNAL_CONTROL_BYTES: usize =
    max_bytes(JOURNAL_BASE_BYTES, journal_runtime_bytes(MAX_STORED_RUNTIME_METADATA_BYTES));
/// One Durable Object SQLite string, BLOB or row holds at most 2 MB
/// (Cloudflare's Durable Object storage limits, the bound
/// `target::MAX_BUNDLE_V2_BYTES` also takes).
pub const DO_SQLITE_VALUE_BYTES: usize = 2_000_000;
/// The largest metadata the signing journal can store and so ever read back:
/// `runtime_cache.metadata_hex` is hex, two characters a byte, in one value
/// of at most [`DO_SQLITE_VALUE_BYTES`]. A fetched runtime above it fails to
/// store, which is AUDIT F5's open stop risk, not something this footprint
/// covers. This follows the stored encoding: a move to a BLOB column doubles
/// it, and with it the journal footprint.
pub const MAX_STORED_RUNTIME_METADATA_BYTES: usize = DO_SQLITE_VALUE_BYTES / 2;
const _: () = assert!(
    MAX_STORED_RUNTIME_METADATA_BYTES <= MAX_RUNTIME_METADATA_BYTES,
    "a stored runtime arrived in one reply"
);
/// A ChainJournal request apart from a stored runtime's read: twice the
/// 486-fixture runtime read's measured 31 pages natively (the margin for
/// wasm-bindgen's string transfer), in whole MiB. The bundle bound
/// ([`SIGNING_BUNDLE_BYTES`]) is half of it.
pub const JOURNAL_BASE_BYTES: usize = 4 * MIB;
/// A signing step's read of a stored runtime of `metadata_len` bytes: the
/// row's hex text and its decoded bytes (measured 31 pages natively for the
/// 411,830 B fixture, scaled by length), twice for wasm-bindgen's string
/// transfer, which holds up to four times the text while it encodes, plus a
/// parse in the request's own scope when the runtimes home has no room for
/// it ([`runtime_parse_bytes`]).
pub const fn journal_runtime_bytes(metadata_len: usize) -> usize {
    let read = (STORED_RUNTIME_READ_BYTES as u128 * metadata_len as u128).div_ceil(STORED_RUNTIME_READ_METADATA as u128);
    let read = if read > (usize::MAX / 2) as u128 { usize::MAX / 2 } else { read as usize };
    (2 * read).saturating_add(runtime_parse_bytes(metadata_len))
}
/// The stored runtime read's measured peak (hex row and decoded bytes,
/// `arena_witness_the_stored_runtime_read_fits_the_journal_footprint`) and
/// the metadata length it was measured with.
pub const STORED_RUNTIME_READ_BYTES: usize = 31 * slop_ninja_arena::PAGE;
pub const STORED_RUNTIME_READ_METADATA: usize = 411_830;

const fn max_bytes(a: usize, b: usize) -> usize {
    if a > b { a } else { b }
}
/// The largest stored reward-v1 bundle a ChainJournal request re-reads
/// within [`JOURNAL_CONTROL_BYTES`]: a signing step holds the active target
/// parsed and verified from it (`signing::active_target`) while the pointer
/// plan and `/status` parse it again, at most half the footprint (the same
/// margin for wasm-bindgen's string transfer), as
/// `SIGNING_BASE_BYTES + SIGNING_PER_BUNDLE_BYTE × bundle`. Both paths that
/// install a bundle (the operator's and the automatic handoff) refuse a
/// larger one (`signing::auto::SIGNED_BUNDLE_BYTES`), so no installed target
/// overdraws a later signing step. Measured natively
/// (`arena_witness_measure_the_signing_reads_of_a_stored_bundle`) and
/// witnessed at the bound
/// (`arena_witness_the_signing_reads_fit_the_journal_footprint_at_the_bundle_bound`).
pub const SIGNING_BUNDLE_BYTES: usize = (JOURNAL_BASE_BYTES / 2 - SIGNING_BASE_BYTES) / SIGNING_PER_BUNDLE_BYTE;
/// Measured 2026-11-04: 2 pages for a 7,363 B bundle, 34 for 505,163 B, 67
/// for 1,002,963 B and 139 for 1,998,763 B (peak live 27,216 B, 1,974,927 B,
/// 3,928,783 B and 7,836,895 B): about 4.6 bytes a bundle byte in whole
/// pages above a two-page base. The base is twice that and the factor
/// rounded up, so the bound (about 367 kB of bundle, 183 kB of a cohort's
/// raw records) sits below the measured line.
pub const SIGNING_BASE_BYTES: usize = 4 * slop_ninja_arena::PAGE;
pub const SIGNING_PER_BUNDLE_BYTE: usize = 5;
/// The automatic-handoff pass (`signing::auto::run_pass`) at its source
/// bound (`signing::auto::SOURCE_BYTES`, the raw records one cohort's bundle
/// can hold): [`auto_pass_bytes`].
pub const JOURNAL_AUTO_BYTES: usize = auto_pass_bytes(crate::signing::auto::SOURCE_BYTES);

/// The automatic-handoff pass's footprint for `source` bytes of a cohort's
/// raw records, as `AUTO_BASE_BYTES + AUTO_PER_SOURCE × source`. Its
/// heaviest step is the handoff's transaction: the records verified, the
/// bundle written once as hex JSON, then parsed, verified and stored by the
/// install; everything in it grows with the records' bytes. Measured
/// natively (`arena_witness_the_automatic_handoff_fits_at_its_source_bound`,
/// 2026-11-04): a whole pass installing a 13,410 B cohort peaks at 8 pages,
/// one of 998,961 B (the bound before installs were held to
/// [`SIGNING_BUNDLE_BYTES`]) at 192 pages (8,837,852 B live), the record
/// reads' exact blocks included: about 12 bytes a source byte above a
/// half-MiB base; the witness now runs at the current, smaller bound. The factor is twice that, the margin
/// `JOURNAL_CONTROL_BYTES` takes for wasm-bindgen's string transfers, and
/// the base four times the measured one (it also holds the listing page and
/// the receipt index's fold buffer, `cf_io::FOLD_BUFFER_BYTES`). Unmeasured
/// on the deployed instance: its peak is on `/status` under
/// `journal/sign-auto`.
pub const fn auto_pass_bytes(source: usize) -> usize {
    AUTO_BASE_BYTES.saturating_add(AUTO_PER_SOURCE.saturating_mul(source))
}
pub const AUTO_BASE_BYTES: usize = 2 * MIB;
pub const AUTO_PER_SOURCE: usize = 24;
/// A cohort-object request: one transaction, with the active plan's copy.
/// Measured: `/plan` with the widest admitted plan 8 pages (its body is an
/// admitted block), `/kick` 2, `/status` 3; a plan at the 512 KiB body bound
/// parses into at most `PLAN_PARSED_BYTES` and is copied once into the plan
/// home, so three times that, in whole MiB.
pub const COHORT_CONTROL_BYTES: usize = 2 * MIB;
/// The draw, close and load-seal steps' footprint until theirs are derived:
/// the alarm's base footprint, in which they ran before they had their own
/// routes (their measured peaks are on `/status`).
pub const STEP_BASE_BYTES: usize = ALARM_CONTROL_BYTES;
/// One live setup or release over a batch of `docs` documents
/// (`eval::live::prepare`, `release_due`): the alarm's base plus, per batch
/// document, its rows (the batch read with its outcome, the partition's
/// keys and the segment update). The base holds the cohort's parsed plan
/// (at most `PLAN_PARSED_BYTES`) and the sealed subjects' digests, at most
/// `MAX_ROSTER` entrants: the step reads only those fields of the stored
/// seal state, never its manifest record (but for `R_k`'s pre-finalized
/// value) or its published index, which grow with the chain's history.
/// Witnesses: one release and the report at the test shape
/// (`arena_witness_live_steps_fit_their_footprint`), and the setup over the
/// full batch, every release of every entrant and `R_k`, and the
/// `/eval/live` report over 40 documents and the same cohort grown to the
/// deployed batch of 1,024, with 40 entrants, the most testnet's roster
/// admits (`arena_witness_live_steps_fit_at_the_deployed_batch`), and at
/// `MAX_ROSTER` (`arena_witness_live_steps_fit_at_the_widest_roster`: at
/// 1,024 documents the heaviest step peaks at 8 of the 34 pages, the report
/// at 14 of 34). Each also checks the per-document growth against
/// [`LIVE_PER_DOC`].
pub const fn live_step_bytes(docs: u64) -> usize {
    let total = (ALARM_CONTROL_BYTES as u64).saturating_add(LIVE_PER_DOC.saturating_mul(docs));
    if total > usize::MAX as u64 { usize::MAX } else { total as usize }
}
/// The live step's footprint per batch document. Measured natively
/// (2026-11-10, the witnesses above): from 40 to 1,024 batch documents at
/// 40 entrants the setup's peak grows by 129 B a document and the heaviest
/// release's by 123 B (at 256 entrants by 47 B and 8 B, where the
/// roster-bound part of the step sets the peak). This is about eight times
/// the larger; the deployed instance (its SQL rows cross from JS) is not
/// measured.
pub const LIVE_PER_DOC: u64 = 1_024;
/// The cohort alarm's base footprint; the seal and package steps admit their
/// shape-derived footprints (`seal_bytes`, `package_bytes`) as child scopes.
/// Measured: picking the next work item over the widest admitted plan, 2
/// pages; the base also holds the stepped plan's copy (`PLAN_PARSED_BYTES`)
/// and a dispatch's kicks, so 1 MiB.
pub const ALARM_CONTROL_BYTES: usize = MIB;
/// An admission step: a files manifest (at most the scorer's
/// `MAX_MANIFEST_BYTES`) rebuilt and serialized, one R2 listing page (the
/// platform's 1,000 keys of at most 1,024 bytes), the step's probes (at
/// most `admission::R2_CALLS_PER_STEP`, each a session id, a key and an
/// entity tag, at most `PROBE_BYTES`), the signer's reply for the canary
/// and the header outcome of a settle. Unmeasured: twice their sum, in
/// whole MiB.
pub const ADMISSION_STEP_BYTES: usize = (2
    * (2 * slop_ninja_artifact_check::files_manifest::MAX_MANIFEST_BYTES
        + 1_000 * 1_024
        + crate::eval::admission::R2_CALLS_PER_STEP * PROBE_BYTES
        + slop_ninja_r2_presign::wire::reply_max(1)))
.div_ceil(MIB)
    * MIB;
/// A reference staging request (`crate::eval::reference`): a Worker
/// route's control (the workers-rs request and response, the signer call),
/// which holds the request's own heap. `commit`: the body at its bound (one
/// unpinned reference manifest), its parse (its strings at most the body's
/// bytes), the pinned manifest's serialization and one head per file of the
/// spec's table. `urls`: the body and its entries (below the commit body),
/// the signer's reply to the whole table and its decoded URLs (each at most
/// the reply), and the answer, both as owned uploads (their strings at most
/// its serialization, [`crate::eval::reference::URLS_REPLY_BYTES`]) and
/// serialized. Unmeasured: the heap is held to half the control (twice the
/// larger route's sum, the admission step's margin); both routes have a
/// `Mode::Strict` witness in this footprint
/// (`arena_witness_reference_{commit,urls}_fits_its_footprint`).
pub const REFERENCE_CONTROL_BYTES: usize = WORKER_CONTROL_BYTES;
const REFERENCE_COMMIT_HEAP_BYTES: usize = 2 * crate::eval::reference::COMMIT_BODY_BYTES
    + crate::eval::reference::MANIFEST_MAX_BYTES
    + crate::eval::reference::MAX_FILES * PROBE_BYTES;
const REFERENCE_URLS_HEAP_BYTES: usize = 2 * crate::eval::reference::URLS_BODY_BYTES
    + 2 * slop_ninja_r2_presign::wire::reply_max(crate::eval::reference::MAX_FILES)
    + 2 * crate::eval::reference::URLS_REPLY_BYTES
    + crate::eval::reference::MAX_FILES * core::mem::size_of::<slop_ninja_admission_wire::json::Upload>();
pub const REFERENCE_HEAP_BYTES: usize = if REFERENCE_COMMIT_HEAP_BYTES > REFERENCE_URLS_HEAP_BYTES {
    REFERENCE_COMMIT_HEAP_BYTES
} else {
    REFERENCE_URLS_HEAP_BYTES
};
const _: () = assert!(2 * REFERENCE_HEAP_BYTES <= REFERENCE_CONTROL_BYTES / 2);
/// One probe's heap at its format bounds: the session id (64), the key
/// (`layout::MAX_KEY_LEN`), an entity tag (`files_manifest::MAX_ETAG_BYTES`)
/// and the struct, rounded up to a power of two.
pub const PROBE_BYTES: usize = 512;
const _: () = assert!(
    64 + slop_ninja_r2_presign::layout::MAX_KEY_LEN + slop_ninja_artifact_check::files_manifest::MAX_ETAG_BYTES + 128
        <= PROBE_BYTES
);
/// One admission event delivery of the Queue consumer: the event message
/// within its bound (`cf_admission::EVENT_MESSAGE_BYTES`), its parse and
/// the cohort object's `observe` call and reply. Unmeasured: the sum is
/// under 64 KiB; rounded up to 256 KiB as the stated margin until measured.
pub const ADMISSION_EVENT_BYTES: usize = 256 * KIB;
/// An admission session operation at the cohort object (`/admission/*`): a
/// cohort request's control, the forwarded message, the session reply at
/// the contract's response limit (built, serialized and relayed: three
/// copies), the header outcome and the signer's reply, and a files
/// manifest built when the session completes (twice: built and
/// serialized). Counted from the code; unmeasured.
pub const fn cohort_session_bytes() -> usize {
    COHORT_CONTROL_BYTES
        + slop_ninja_admission_wire::internal::FORWARD_BYTES
        + 3 * slop_ninja_admission_wire::RESPONSE_BASE_BYTES
        + 4 * KIB
        + slop_ninja_r2_presign::wire::reply_max(1 + slop_ninja_artifact_check::spec::ROLES.len())
        + 2 * slop_ninja_artifact_check::files_manifest::MAX_MANIFEST_BYTES
}

/// A commitment-reader round: the parse of the stored runtime (scaled from
/// the measured fixture parse by the largest metadata a reply can carry,
/// half a large reply's hex) and the round's body, decoded twice
/// (`cf_admission::READER_BODY_BYTES`). Unmeasured.
pub const COMMITMENTS_CONTROL_BYTES: usize =
    runtime_parse_bytes(MAX_RUNTIME_METADATA_BYTES) + 4 * crate::rpc::MAX_LARGE_RESPONSE_BYTES;
/// A Durable Object constructor: its storage migration.
pub const CONSTRUCTOR_CONTROL_BYTES: usize = 256 * KIB;
/// The smallest route footprint: the model's `min_route_bytes`, from which
/// the arena derives its glue tickets (checked by a test).
pub const MIN_CONTROL_BYTES: usize = CONSTRUCTOR_CONTROL_BYTES;

/// The package step's footprint for `jobs` jobs and `docs` documents.
///
/// An upper bound fitted to the measured linear memory of the package step
/// (design §1.3, probe harness on 2026-10-31, n = 1,000: 29.56, 44.38, 83.31
/// and 177.31 MiB at J = 41, 65, 129 and 257; J = 129, n = 4,000: 245.31
/// MiB) as `base + per_receipt × J × n + per_pair × J²` through the
/// J = 65, 129 and 257 points (rounded up): it over-estimates the J = 41 and
/// the n = 4,000 points. The J² term is the pairwise `rms` JSON. Version
/// `package-fit-2026-10-31`; re-fitted when the streaming package lands.
pub const fn package_bytes(jobs: u64, docs: u64) -> usize {
    const BASE: u64 = 10_852_353; // 10.35 MiB
    const PER_RECEIPT: u64 = 505;
    const PER_JOB_PAIR: u64 = 689;
    let receipts = jobs.saturating_mul(docs);
    let total = BASE
        .saturating_add(PER_RECEIPT.saturating_mul(receipts))
        .saturating_add(PER_JOB_PAIR.saturating_mul(jobs.saturating_mul(jobs)));
    if total > usize::MAX as u64 { usize::MAX } else { total as usize }
}

/// What a cohort sealed in the seeded task order (`reward_v1.task_order`)
/// holds beside the package step's measured footprint while its job vectors
/// are resident: the slot -> task layout and the permutation's visited
/// flags, one of each per document (`eval::v3::prepare_package`, which
/// permutes each entrant's vectors in place and reads an appended cohort
/// exactly as before). Derived from the types, not measured: the package
/// fit's margin over each measured point covers it
/// (`the_package_fit_covers_the_seeded_layout`). The layout's sort scratch
/// is freed before the first job vector is built, while the resident
/// summaries alone are smaller than the vectors they become.
pub const fn seeded_layout_bytes(docs: u64) -> usize {
    let per = (core::mem::size_of::<usize>() + core::mem::size_of::<bool>()) as u64;
    let total = docs.saturating_mul(per);
    if total > usize::MAX as u64 { usize::MAX } else { total as usize }
}

/// The seal step's footprint for `docs` documents: fitted to the measured
/// 12.69 MiB (n = 1,000) and 47.44 MiB (n = 4,000) of 1.3 KB texts (design
/// §1.3) as `base + per_doc × n`, rounded up. Version `seal-fit-2026-10-31`.
pub const fn seal_bytes(docs: u64) -> usize {
    const BASE: u64 = 1_158_676; // 1.105 MiB
    const PER_DOC: u64 = 12_148;
    let total = BASE.saturating_add(PER_DOC.saturating_mul(docs));
    if total > usize::MAX as u64 { usize::MAX } else { total as usize }
}

/// The pages a scope of `control` footprint bytes holds: its state page and
/// one heap run (what `Scope::open` and `ScopeRef::child` admit).
pub const fn scope_bytes(control: usize) -> usize {
    let run = (control.saturating_add(slop_ninja_arena::SEGMENT_OVERHEAD)).next_multiple_of(slop_ninja_arena::PAGE);
    slop_ninja_arena::scope_pages(run) as usize * slop_ninja_arena::PAGE
}

/// R2 operations a cohort step keeps in flight at once (`cf_eval`'s
/// `PARALLEL`): the package step reads its job summaries this many at a
/// time, each into its own admitted block.
pub const STEP_READS_IN_FLIGHT: usize = 6;

/// The largest object any cohort step reads whole and holds in its scope,
/// for the kinds whose schema sets no tighter bound (pool manifests and
/// shards, used-ledger entries, load-seal source indexes, probe records and
/// shards, artifact files manifests): half the memory model's per-invocation
/// JS envelope (`js_envelope_bytes`), the design's ceiling for a body a route
/// holds whole. The largest such object on testnet is a pool shard of about
/// 1.2 MB.
pub const STEP_OBJECT_BYTES: usize = slop_ninja_arena::model::JS_ENVELOPE_BYTES / 2;

/// The widest JSON one task adds to a job summary: one element in each of
/// the three per-task arrays (`share_ppb` and `inv_ppb` as `4294967295,`,
/// `roles` as `false,`).
pub const SUMMARY_TASK_BYTES: usize = 11 + 11 + 6;

/// A job summary's bound for a job of `tasks` tasks: the fixed fields and
/// the components (`eval::engine::SUMMARY_FIXED_BYTES`) and
/// `SUMMARY_TASK_BYTES` per task, capped at `STEP_OBJECT_BYTES`.
pub const fn summary_bytes(tasks: u64) -> usize {
    let b = (crate::eval::engine::SUMMARY_FIXED_BYTES as u64).saturating_add(tasks.saturating_mul(SUMMARY_TASK_BYTES as u64));
    if b > STEP_OBJECT_BYTES as u64 { STEP_OBJECT_BYTES } else { b as usize }
}

/// The package step's `quant-v1` work beside the fitted heap: the public
/// `QuantizedResult` built from the finalized package (with the q-only
/// outcome it recomputes), its digest, and its signed bytes beside a copy of
/// the record, held at once while the step publishes them. An upper bound
/// `base + per_entrant x E` over the peaks measured on the arena
/// (`arena_witness_the_quantized_result_fits_its_derived_footprint`, native
/// 64-bit sizes, which bound wasm32's from above; 2026-11-05: 15,878,
/// 62,178 and 123,786 B at E = 16, 64 and 128, about 963 B an entrant): the
/// per-entrant term is the measured slope with a fifth added for the credit
/// fractions' width, which grows with the band layout, rounded up to a
/// multiple of 64. Version `quant-fit-2026-11-05`.
pub const fn quant_package_bytes(entrants: u64) -> usize {
    const BASE: u64 = 16 * 1024;
    const PER_ENTRANT: u64 = 1_216;
    let total = BASE.saturating_add(PER_ENTRANT.saturating_mul(entrants));
    if total > usize::MAX as u64 { usize::MAX } else { total as usize }
}

/// A batch text's allowance in the span gate's trivial-policy pass: 16 KiB,
/// about eight bytes a token at the scorer's 2,048-token input bound
/// (testnet batch texts are about 1.3 KB, the seal fit's). An estimate, not
/// a bound the store enforces.
pub const SPAN_TEXT_BYTES: u64 = 16 * 1024;

/// The span gate's evaluation at packaging (`eval::span_gate::evaluate`),
/// for a plan naming `reward_v1.localization`, beside the package step's
/// fitted heap. Per job: its aggregate row (13 columns), its excess and
/// load-fact rows (the facts' rule name at most
/// `span_gate::DECODER_RULE_CHARS`), its statistics and its private
/// components entry, about 4 KiB, doubled. Once: the trivial policies'
/// grid and aggregates (273 of each), the anchor's components and the
/// record, in 64 KiB; and one page of `span_gate::TRIVIAL_PAGE` batch texts
/// at `SPAN_TEXT_BYTES` (each with its row) beside one document's sentences
/// and geometry (64 bytes a sentence, at most one sentence per two bytes).
/// Derived from the types at native 64-bit sizes, which bound wasm32's; not
/// measured beyond the witness `arena_witness_the_span_gate_fits_its_footprint`
/// at its test shape.
pub const fn span_gate_bytes(jobs: u64) -> usize {
    const BASE: u64 = 64 * 1024;
    const PER_JOB: u64 = 8 * 1024;
    const ROW: u64 = 512;
    let page = (crate::eval::span_gate::TRIVIAL_PAGE as u64).saturating_mul(SPAN_TEXT_BYTES.saturating_add(ROW));
    let one_doc = SPAN_TEXT_BYTES.saturating_mul(32);
    let total = BASE.saturating_add(PER_JOB.saturating_mul(jobs)).saturating_add(page).saturating_add(one_doc);
    if total > usize::MAX as u64 { usize::MAX } else { total as usize }
}

/// The package step's child scope: its fitted heap (`package_bytes`), its
/// quantization (`quant_package_bytes`, for every job but `R_k`'s) and, for
/// a span cohort (`span`), the span gate's evaluation (`span_gate_bytes`).
/// The summaries it reads are separate exact blocks (`package_reads_bytes`).
pub const fn package_step_bytes(jobs: u64, docs: u64, span: bool) -> usize {
    let step = package_bytes(jobs, docs).saturating_add(quant_package_bytes(jobs.saturating_sub(1)));
    if span { step.saturating_add(span_gate_bytes(jobs)) } else { step }
}

/// The block runs the package step's own reads hold at once: at most
/// `STEP_READS_IN_FLIGHT` summaries of a job of `tasks` tasks (a returned
/// run is re-keyed by the next read, so the count never grows with the
/// jobs).
pub const fn package_reads_bytes(tasks: u64) -> usize {
    STEP_READS_IN_FLIGHT.saturating_mul(slop_ninja_arena::block_bytes(summary_bytes(tasks), 1))
}

/// What a package step's invocation holds: the alarm's scope, the step's
/// child scope and its in-flight summary blocks.
pub const fn package_invocation_bytes(jobs: u64, docs: u64, tasks: u64, span: bool) -> usize {
    scope_bytes(ALARM_CONTROL_BYTES)
        .saturating_add(scope_bytes(package_step_bytes(jobs, docs, span)))
        .saturating_add(package_reads_bytes(tasks))
}

/// The largest package the model holds: `largest_route_bytes` of
/// `memory-model.json` is the package invocation at the load instance's
/// largest rehearsed shape (R_0 + 128 jobs, n = 1,000, 1,000 tasks a job),
/// so a plan whose package would need more is refused at `/plan`
/// (`arena_capacity`), never admitted to fail at packaging.
pub const fn package_fits(jobs: u64, docs: u64, tasks: u64, span: bool) -> bool {
    package_invocation_bytes(jobs, docs, tasks, span) <= slop_ninja_arena::model::LARGEST_ROUTE_BYTES
}

/// The home scopes' footprints. Durable Object state: every co-resident
/// ChainJournal and EvalCohort object (handles, settings and the signing
/// configuration), at most the model's resident count
/// (`model::GLUE_RESIDENTS`, the constructor's admission) at
/// `HOME_STATE_INSTANCE_BYTES` each. Runtimes: the ChainJournal's
/// parsed runtimes (`signing::PARSED_RUNTIMES` of them). Plans: the cohort
/// object's cached plans (`eval::engine::PLAN_CACHE_PLANS` of them). Each is
/// opened only where it is used, so the eval Worker holds no runtimes home
/// unless its journal signs. Witness: `arena_witness_home_caches_fit` holds
/// each cache at its entry bound under `Strict` in exactly its footprint.
pub const HOME_STATE_BYTES: usize =
    slop_ninja_arena::model::GLUE_RESIDENTS as usize * HOME_STATE_INSTANCE_BYTES + slop_ninja_arena::SEGMENT_OVERHEAD;
/// One object's state in the State home: measured 416 B live (the largest,
/// a ChainJournal with its signing configuration, local smoke run,
/// 2026-11-01; an EvalCohort holds 24 B on testnet), rounded up to the next
/// power of two as the margin for its chunk headers, as the glue probe's
/// per-entry inputs are.
pub const HOME_STATE_INSTANCE_BYTES: usize = 512;
/// The runtimes home holds `PARSED_RUNTIMES` entries at their parse peak
/// and one parse beside them: a chain upgrade with a full cache evicts one
/// entry and parses the next while the other stays resident, and the
/// evicted entry's freed chunks are scattered through the heap, so the
/// home is sized for every entry at its peak plus the new parse
/// (`arena_witness_a_third_runtime_loads_into_a_full_cache_within_the_home`).
pub const HOME_RUNTIMES_BYTES: usize = RUNTIME_PARSED_BYTES * (crate::signing::PARSED_RUNTIMES + 1) + slop_ninja_arena::SEGMENT_OVERHEAD;
pub const HOME_PLANS_BYTES: usize = PLAN_PARSED_BYTES * crate::eval::engine::PLAN_CACHE_PLANS + slop_ninja_arena::SEGMENT_OVERHEAD;
/// Measured parse peaks of recorded runtimes and the length-scaled need of
/// a parse, shared with the upload Worker, which parses the same metadata
/// with the same rtao-core parser (`slop_ninja_arena::measured`).
pub use slop_ninja_arena::measured::{RUNTIME_PARSE_MEASUREMENTS, largest_recorded_metadata, runtime_parse_bytes};

/// The largest metadata an RPC reply can carry: a `0x` hex string within
/// `rpc::MAX_LARGE_RESPONSE_BYTES` holds at most half that many bytes. A
/// reader that parses a reply directly is bounded by it; the signing
/// journal's stored runtimes are bounded more tightly by their row
/// ([`MAX_STORED_RUNTIME_METADATA_BYTES`]).
pub const MAX_RUNTIME_METADATA_BYTES: usize = crate::rpc::MAX_LARGE_RESPONSE_BYTES / 2;

/// One parsed chain runtime in the runtimes home: the parse of the largest
/// recorded runtime, scaled ([`runtime_parse_bytes`]). The cache evicts down
/// to `PARSED_RUNTIMES - 1` before it parses, admits a parse by its own
/// scaled need, and parses uncached when the home lacks room, so a larger
/// runtime than any recorded one still parses (in the request's scope) and
/// is simply not cached.
pub const RUNTIME_PARSED_BYTES: usize = runtime_parse_bytes(largest_recorded_metadata());

/// One cached plan at the largest accepted plan body: the measured peak
/// pages of the cache's copy (key and plan) of the widest plan whose JSON
/// fits `MAX_PLAN_BODY_BYTES` (`arena_witness_home_caches_fit`).
pub const PLAN_PARSED_BYTES: usize = 10 * slop_ninja_arena::PAGE;

/// The largest plan body `/plan` accepts.
pub const MAX_PLAN_BODY_BYTES: usize = 512 * KIB;

/// The isolate's home scopes: bounded memory for what outlives an
/// invocation, the Durable Objects' own state and the two in-memory caches
/// (the parsed chain runtimes and the parsed plans). Each cache bounds its
/// entries, every entry is admitted against its home's footprint, and an
/// entry that would not fit is simply not cached (a cache miss re-reads
/// storage, so behaviour is unchanged). Everything a request allocates
/// otherwise stays in its own scope.
pub mod home {
    use super::{HOME_PLANS_BYTES, HOME_RUNTIMES_BYTES, HOME_STATE_BYTES, R_HOME_PLANS, R_HOME_RUNTIMES, R_HOME_STATE};
    use slop_ninja_arena::{Route, Scope};

    /// Which home.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Home {
        State = 0,
        Runtimes = 1,
        Plans = 2,
    }

    pub const ALL: [Home; 3] = [Home::State, Home::Runtimes, Home::Plans];

    impl Home {
        pub const fn route(self) -> Route {
            match self {
                Home::State => R_HOME_STATE,
                Home::Runtimes => R_HOME_RUNTIMES,
                Home::Plans => R_HOME_PLANS,
            }
        }

        pub const fn bytes(self) -> usize {
            match self {
                Home::State => HOME_STATE_BYTES,
                Home::Runtimes => HOME_RUNTIMES_BYTES,
                Home::Plans => HOME_PLANS_BYTES,
            }
        }
    }

    /// Each home's scope once it opened, and the opens refused so far.
    type Slots = ([Option<Scope>; 3], [u32; 3]);

    #[cfg(target_arch = "wasm32")]
    struct Cell(core::cell::UnsafeCell<Slots>);
    // SAFETY: wasm32 without atomics is single-threaded.
    #[cfg(target_arch = "wasm32")]
    unsafe impl Sync for Cell {}
    #[cfg(target_arch = "wasm32")]
    static HOMES: Cell = Cell(core::cell::UnsafeCell::new(([None, None, None], [0; 3])));

    #[cfg(not(target_arch = "wasm32"))]
    std::thread_local! {
        static HOMES: core::cell::RefCell<Slots> = const { core::cell::RefCell::new(([None, None, None], [0; 3])) };
    }

    fn with_slots<R>(f: impl FnOnce(&mut Slots) -> R) -> R {
        #[cfg(target_arch = "wasm32")]
        {
            // SAFETY: single-threaded; no reference outlives this call, and
            // no caller re-enters (the closures never call `enter`).
            f(unsafe { &mut *HOMES.0.get() })
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            HOMES.with(|cell| f(&mut cell.borrow_mut()))
        }
    }

    /// Runs `f` in `home`, opened with its footprint on the first use that
    /// can admit it. When the scope cannot be admitted, `f` is handed back
    /// unrun (`Err(f)`): a cache then does not cache, and a Durable Object
    /// builds its state with no scope current. A refused open is counted
    /// (`open_failures`, on `/status`) and tried again at the next use (an
    /// allocation-free admission), so a home refused while a large step
    /// held the pool opens once the pool has room. `f` must not await.
    pub fn enter<R, F: FnOnce() -> R>(home: Home, f: F) -> Result<R, F> {
        with_slots(|(slots, failures)| {
            let i = home as usize;
            let (Some(slot), Some(failed)) = (slots.get_mut(i), failures.get_mut(i)) else { return Err(f) };
            if slot.is_none() {
                match Scope::open(home.route(), home.bytes()) {
                    Ok(s) => {
                        // A home never closes: marked permanent, a refusal
                        // its pages cause is classed `Capacity`, never
                        // `Retry`.
                        s.set_permanent();
                        *slot = Some(s);
                    }
                    Err(_) => *failed = failed.saturating_add(1),
                }
            }
            match slot.as_ref() {
                Some(s) => Ok(s.enter(f)),
                None => Err(f),
            }
        })
    }

    /// A home's accounting, if it is open.
    pub fn stats(home: Home) -> Option<slop_ninja_arena::ScopeStats> {
        with_slots(|(slots, _)| slots.get(home as usize).and_then(|s| s.as_ref()).map(|s| s.stats()))
    }

    /// How many times opening `home` was refused.
    pub fn open_failures(home: Home) -> u32 {
        with_slots(|(_, failures)| failures.get(home as usize).copied().unwrap_or(0))
    }

    /// The bytes `home` can still hold: its reservation, less its state page
    /// and its live bytes (0 when it is not open yet: it opens with its
    /// whole footprint, so callers that enter it first see the opened value).
    pub fn free_bytes(home: Home) -> usize {
        stats(home).map_or(0, |st| {
            (st.reserved_pages as usize).saturating_sub(1).saturating_mul(slop_ninja_arena::PAGE).saturating_sub(st.live)
        })
    }
}

/// The static data and stack the fit check below assumes: 48 pages, the
/// 1 MiB linked stack plus 2 MiB of data. Rule (memory model
/// `coordinator-2026-11-04-r1`): the measured static end, page-rounded, plus
/// a stated margin. Measured 2026-11-04 on the release wasm32 module of
/// commit f562a6d6, with the compiled-in scorer token table
/// (`slop-ninja-scorer-tokens`, 408,878 B of it), by
/// `examples/arena_wasm_check`: static end 2,173,704 B, 34 pages
/// page-rounded; the margin is the remaining 14 pages (917,504 B) for later
/// growth. Both fit checks below hold at it (the build checks them, and
/// `tests::the_static_bound_leaves_room_in_both_workers_fits` prints the room
/// they leave). The gate on the module itself is `examples/arena_wasm_check`,
/// which fails when the built module's static end exceeds this bound
/// (`__heap_base` when exported, else the end of its data segments, a lower
/// bound: worker-build's output keeps no `__heap_base`); the smoke run checks
/// the deployed layout's `static_bytes` against it.
pub const STATIC_UPPER_BYTES: usize = slop_ninja_arena::model::STACK_BYTES + 2 * MIB;

/// The room the eval Worker's package fit and the coordinator Worker's
/// handoff fit leave beside [`STATIC_UPPER_BYTES`] (checked non-negative at
/// build time by the assertions below).
pub const fn static_fit_room() -> (usize, usize) {
    use slop_ninja_arena::model;
    let glue = model::GLUE_BUDGET_PAGES as usize * slop_ninja_arena::PAGE;
    let eval = STATIC_UPPER_BYTES + glue + scope_bytes(HOME_STATE_BYTES) + scope_bytes(HOME_PLANS_BYTES) + model::LARGEST_ROUTE_BYTES + scope_bytes(COHORT_CONTROL_BYTES);
    let coord = STATIC_UPPER_BYTES
        + glue
        + scope_bytes(HOME_STATE_BYTES)
        + scope_bytes(HOME_RUNTIMES_BYTES)
        + scope_bytes(JOURNAL_AUTO_BYTES)
        + scope_bytes(JOURNAL_CONTROL_BYTES)
        + scope_bytes(CONTINUATION_CONTROL_BYTES);
    (model::LINEAR_MEMORY_BYTES.saturating_sub(eval), model::LINEAR_MEMORY_BYTES.saturating_sub(coord))
}

// The memory model and this route table agree, checked at every build.
const _: () = {
    use slop_ninja_arena::model;
    // memory-model.json `min_route_bytes` is the smallest route footprint
    // (the arena derives its glue tickets from it).
    assert!(model::MIN_ROUTE_BYTES == MIN_CONTROL_BYTES);
    // `largest_route_bytes` is the package invocation at R_0 + 128,
    // n = 1,000: that shape fits, J = 257 at n = 1,000 is refused at `/plan`.
    assert!(model::LARGEST_ROUTE_BYTES == package_invocation_bytes(129, 1_000, 1_000, false));
    assert!(package_fits(129, 1_000, 1_000, false));
    assert!(!package_fits(257, 1_000, 1_000, false));
    // The eval Worker during that package: static data and the stack (at
    // most `STATIC_UPPER_BYTES`, checked on the built module), the glue
    // partition, its state and plans homes, and room for a concurrent cohort
    // request. An admission session operation (`cohort_session_bytes`) is
    // not one of them: it runs only while its cohort admits or verifies, and
    // cohorts advance one at a time (`engine::advance` moves only the first
    // unclosed cohort; the freeze waits for every verification), so no
    // package step runs then. Its own fit follows.
    let static_upper = STATIC_UPPER_BYTES;
    let glue = model::GLUE_BUDGET_PAGES as usize * slop_ninja_arena::PAGE;
    let homes = scope_bytes(HOME_STATE_BYTES) + scope_bytes(HOME_PLANS_BYTES);
    assert!(static_upper + glue + homes + model::LARGEST_ROUTE_BYTES + scope_bytes(COHORT_CONTROL_BYTES) <= model::LINEAR_MEMORY_BYTES);
    // The coordinator Worker during an automatic-handoff pass at its source
    // bound: its state and runtimes homes, the pass, a concurrent journal
    // request and the cron tick's continuation beside it.
    let homes = scope_bytes(HOME_STATE_BYTES) + scope_bytes(HOME_RUNTIMES_BYTES);
    let beside = scope_bytes(JOURNAL_CONTROL_BYTES) + scope_bytes(CONTINUATION_CONTROL_BYTES);
    assert!(static_upper + glue + homes + scope_bytes(JOURNAL_AUTO_BYTES) + beside <= model::LINEAR_MEMORY_BYTES);
    assert!(JOURNAL_AUTO_BYTES <= model::LARGEST_ROUTE_BYTES);
    // A reference staging request beside the eval Worker's package fit and
    // its admitting fit below: the operator may stage while a cohort packages
    // or admits.
    assert!(REFERENCE_CONTROL_BYTES <= model::LARGEST_ROUTE_BYTES);
    let homes = scope_bytes(HOME_STATE_BYTES) + scope_bytes(HOME_PLANS_BYTES);
    assert!(
        static_upper + glue + homes + model::LARGEST_ROUTE_BYTES + scope_bytes(COHORT_CONTROL_BYTES) + scope_bytes(REFERENCE_CONTROL_BYTES)
            <= model::LINEAR_MEMORY_BYTES
    );
    // The eval Worker while a cohort admits: its homes, the alarm with an
    // admission step, a commitment-reader round, an ordinary cohort request
    // and an admission session operation at their footprints, and the Queue
    // consumer at its concurrency (`admission::EVENTS_MAX_CONCURRENCY`
    // batches, each its control and one event delivery at a time; the same
    // isolate may run both).
    let homes = scope_bytes(HOME_STATE_BYTES) + scope_bytes(HOME_PLANS_BYTES);
    let object = scope_bytes(ALARM_CONTROL_BYTES)
        + scope_bytes(ADMISSION_STEP_BYTES)
        + scope_bytes(COMMITMENTS_CONTROL_BYTES)
        + scope_bytes(COHORT_CONTROL_BYTES)
        + scope_bytes(cohort_session_bytes());
    let consumer = crate::eval::admission::EVENTS_MAX_CONCURRENCY
        * (scope_bytes(QUEUE_CONTROL_BYTES) + scope_bytes(ADMISSION_EVENT_BYTES));
    // An operator's reference staging request in the same isolate.
    let staging = scope_bytes(REFERENCE_CONTROL_BYTES);
    assert!(static_upper + glue + homes + object + consumer + staging <= model::LINEAR_MEMORY_BYTES);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_package_fit_bounds_every_measured_point() {
        const MIB_F: f64 = 1_048_576.0;
        for (jobs, docs, measured_mib) in [(41, 1_000, 29.56), (65, 1_000, 44.38), (129, 1_000, 83.31), (257, 1_000, 177.31), (129, 4_000, 245.31)] {
            let fit = package_bytes(jobs, docs) as f64 / MIB_F;
            assert!(fit >= measured_mib, "J = {jobs}, n = {docs}: fit {fit:.2} MiB below the measured {measured_mib} MiB");
            assert!(fit <= measured_mib * 1.12, "J = {jobs}, n = {docs}: fit {fit:.2} MiB far above the measured {measured_mib} MiB");
        }
        for (docs, measured_mib) in [(1_000, 12.69), (4_000, 47.44)] {
            let fit = seal_bytes(docs) as f64 / MIB_F;
            assert!(fit >= measured_mib && fit <= measured_mib * 1.02, "seal n = {docs}: fit {fit:.2} MiB, measured {measured_mib}");
        }
    }

    #[test]
    fn the_quant_fit_bounds_every_measured_point() {
        for (entrants, measured) in [(16u64, 15_878usize), (64, 62_178), (128, 123_786)] {
            let fit = quant_package_bytes(entrants);
            assert!(fit >= measured, "E = {entrants}: fit {fit} B below the measured {measured} B");
            assert!(fit <= measured * 5 / 2, "E = {entrants}: fit {fit} B far above the measured {measured} B");
        }
    }

    #[test]
    fn the_package_fit_covers_the_seeded_layout() {
        const MIB_F: f64 = 1_048_576.0;
        for (jobs, docs, measured_mib) in [(41, 1_000, 29.56), (65, 1_000, 44.38), (129, 1_000, 83.31), (257, 1_000, 177.31), (129, 4_000, 245.31)] {
            // The measurements carry two decimals of a MiB.
            let margin = package_bytes(jobs, docs) as f64 - (measured_mib + 0.005) * MIB_F;
            assert!(
                margin >= seeded_layout_bytes(docs) as f64,
                "J = {jobs}, n = {docs}: margin {margin:.0} B below the seeded layout's {} B",
                seeded_layout_bytes(docs)
            );
        }
    }

    #[test]
    fn the_static_bound_leaves_room_in_both_workers_fits() {
        let (eval, coord) = static_fit_room();
        eprintln!("static bound {STATIC_UPPER_BYTES} B: room beside the eval package fit {eval} B, beside the coordinator handoff fit {coord} B");
        eprintln!("cohort session request {} B (scope {} B), cohort control scope {} B", cohort_session_bytes(), scope_bytes(cohort_session_bytes()), scope_bytes(COHORT_CONTROL_BYTES));
        assert!(eval > 0 && coord > 0);
    }

    #[test]
    fn the_build_links_the_models_fixed_linear_memory() {
        assert_eq!(
            env!("SLOP_NINJA_LINEAR_MEMORY_BYTES").parse::<usize>().ok(),
            Some(slop_ninja_arena::model::LINEAR_MEMORY_BYTES),
            "build.rs links the model's fixed linear memory"
        );
    }
}

//! Generation v2 in the cohort (design `gen-v2`, sections 3-10): the draw's
//! rotation, the lanes' requests, the commit's outcome classes, vendor
//! pause and fallback, spend by vendor, and the private generation records
//! sealed beside the batch.
//!
//! Everything here runs only for a plan naming `generation`
//! ([`crate::eval::plan::CohortPlan::generation`]). This build still refuses
//! such a plan at validation ([`crate::eval::plan::GENERATION_V2_REFUSAL`]):
//! the flag stays off until the generator manifest is measured, the keys
//! exist and the user approves; native tests drive the path directly.
//!
//! - The draw ([`Draw`]) loads the plan's bound generator manifest and voice
//!   library (checked against their digests and kept in the cohort's store,
//!   `gen_objects`, so every later step reads exactly what the draw used),
//!   assigns each generated document a cell from its stream's durable decks
//!   (`gen_rotation`, advanced inside the draw transaction) and a seeded
//!   voice under the plan's split, and records the assignment in the
//!   document's private generation record (`docs.gen_record`).
//! - A claim ([`call_for`]) builds the vendor request of the document's
//!   current cell from the stored record; the lane routes it (frontier
//!   through the generation Worker, Workers AI through the binding) and
//!   commits the vendor's reply.
//! - A commit ([`apply_one`]) records every attempt and its charge, then acts
//!   on the outcome class (`slop_ninja_gen_core::pause::decide`): validate and
//!   accept, retry the next cell of the lane, back off, move a frontier item
//!   to Workers AI, pause a vendor deployment-wide, or disable a cell.
//! - The seal ([`seal_records`]) writes one record per generated document to
//!   private shards bound by digest in the batch index.
//!
//! Nothing here reads a runtime version, keeps vendor message text or a key,
//! or publishes anything: the records, the assignment and the per-vendor
//! spend stay in the private store, the private eval bucket and the
//! operator's status.
#![cfg_attr(
    not(test),
    deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

use crate::eval::engine::{self, Cohort, EvalError, MixedSpec, R, ScratchSpec, i, item_digest, opt_text, s, sha_hex};
use crate::eval::textgen::{self, Reject};
use crate::sql::{Row, Sql, SqlValue, bind_u64, int, text, uint};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use slop_ninja_gen_core::manifest::{Manifest, ModelEntry, ObjectRef};
use slop_ninja_gen_core::outcome::{GenCall, GenReply, OutcomeCode};
use slop_ninja_gen_core::pause::{self, Action, VendorStates};
use slop_ninja_gen_core::plan::GenerationPlan;
use slop_ninja_gen_core::record::{AttemptRecord, FinalRecord, GenRecord};
use slop_ninja_gen_core::request::{self, GenTarget};
use slop_ninja_gen_core::rotation::{self, Avoid, FallbackCause, GenAssignment, StreamState};
use slop_ninja_gen_core::spend;
use slop_ninja_gen_core::vendor::{DocKind, Effort, Lane, Vendor};
use slop_ninja_gen_core::voice::{Library, VoiceChoice, VoiceSplit};
use std::collections::{BTreeMap, BTreeSet};

/// v26: generation v2's private state in the cohort store:
/// - `docs.gen_record`: a generated document's assignment, current cell and
///   every attempt ([`DocGen`]); absent on every v1 document;
/// - `gen_objects`: the generator manifest and voice library a cohort drew
///   with, exactly as bound by the plan, one row each (their sum within
///   [`GEN_OBJECTS_BYTES`]);
/// - `gen_rotation`: each stream's durable deck state (one JSON row per
///   stream; streams span cohorts and are never reset by time);
/// - `gen_vendor_state`: the deployment-wide vendor pause (one row);
/// - `gen_spend`: per cohort, vendor, model and effort: calls, tokens and
///   cost, so spend is reported and settled by vendor;
/// - `gen_disabled`: (model, effort) cells a vendor refused as requests (400)
///   for the rest of the cohort;
/// - `gen_results`: each call whose result was recorded, by its claim token
///   (one per call), so an exact redelivery of a commit records nothing a
///   second time.
pub const SCHEMA_V26: &[&str] = &[
    "ALTER TABLE docs ADD COLUMN gen_record TEXT",
    "CREATE TABLE gen_objects (
        seq INTEGER NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('manifest', 'library')), body TEXT NOT NULL,
        sha256 TEXT NOT NULL, PRIMARY KEY (seq, kind)
    ) STRICT",
    "CREATE TABLE gen_rotation (stream TEXT PRIMARY KEY, state TEXT NOT NULL, updated_seq INTEGER NOT NULL) STRICT",
    "CREATE TABLE gen_vendor_state (id INTEGER PRIMARY KEY CHECK (id = 1), state TEXT NOT NULL) STRICT",
    "CREATE TABLE gen_spend (
        seq INTEGER NOT NULL, vendor TEXT NOT NULL, model TEXT NOT NULL, effort TEXT NOT NULL,
        calls INTEGER NOT NULL DEFAULT 0, unmeasured_calls INTEGER NOT NULL DEFAULT 0,
        input_tokens INTEGER NOT NULL DEFAULT 0, cached_input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0, reasoning_tokens INTEGER NOT NULL DEFAULT 0,
        cost_nanousd INTEGER NOT NULL DEFAULT 0, cost_microusd INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (seq, vendor, model, effort)
    ) STRICT",
    "CREATE TABLE gen_disabled (
        seq INTEGER NOT NULL, model TEXT NOT NULL, effort TEXT NOT NULL, PRIMARY KEY (seq, model, effort)
    ) STRICT",
    "CREATE TABLE gen_results (seq INTEGER NOT NULL, idx INTEGER NOT NULL, claim INTEGER NOT NULL, PRIMARY KEY (seq, idx, claim)) STRICT",
];

/// The largest string a Durable Object's SQLite stores in one row (the
/// platform's documented 2 MB limit for a string, BLOB or row). Each bound
/// object is one row; [`GEN_OBJECTS_BYTES`] keeps both far below it.
pub const STORED_OBJECT_BYTES: u64 = 2_000_000;

/// The largest generator manifest and voice library together (declared
/// bytes, `gen_objects_v1`). Every generation claim and commit (the cohort
/// object's fetch, `arena_routes::COHORT_CONTROL_BYTES`) reads both rows
/// and parses them; the bound gives them half of that footprint (the other
/// half stays with the route's own control and the claim's items), at
/// twice the measured parse peak per byte ([`GEN_OBJECTS_PARSE_FACTOR`]),
/// the margin other footprints take for wasm-bindgen's string transfers
/// (the rows cross from JavaScript). The draw refuses a plan whose objects
/// pass it before reading them, so no later step meets a context it cannot
/// hold. A larger library needs a new bound version (a smaller per-claim
/// context, or a larger route footprint), not a silent overdraft.
pub const GEN_OBJECTS_BYTES: u64 = (crate::arena_routes::COHORT_CONTROL_BYTES as u64 / 2) / (2 * GEN_OBJECTS_PARSE_FACTOR);
/// The peak bytes of a loaded generation context per declared object byte
/// (both rows read, parse transients and the parsed structures), rounded
/// up. Measured natively on 2026-11-05
/// (`gen_v2_tests::witness::arena_witness_gen_objects_measure`): the
/// fixture manifest with a library of fixture-length entries peaked at
/// 208,364 B live for 87,252 declared bytes (2.39 B a byte) and at
/// 395,681 B for 174,409 (2.27, the bound). The claim and
/// commit witnesses run at [`GEN_OBJECTS_BYTES`] under the route's exact
/// footprint, `Strict`.
pub const GEN_OBJECTS_PARSE_FACTOR: u64 = 3;
/// What the draw step admits beside its base footprint for a v2 plan: the
/// two objects read as exact blocks, the manifest parsed once by the Worker
/// for the library's binding, and both parsed again and stored by the draw
/// (`Draw::begin`), at [`GEN_OBJECTS_BYTES`]: the bytes once and two
/// parses at twice the measured factor (witnessed natively,
/// `arena_witness_the_draw_holds_the_generation_objects_within_its_addition`).
pub const GEN_DRAW_BYTES: usize = (GEN_OBJECTS_BYTES * (1 + 2 * 2 * GEN_OBJECTS_PARSE_FACTOR)) as usize;
const _: () = assert!(GEN_OBJECTS_BYTES < STORED_OBJECT_BYTES);

/// The batch index schema of a cohort that ran generation v2: the earlier
/// index (v2, or v3 with the seeded order) plus the `generation` block. One
/// name covers both: the seeded order applies exactly when the index
/// carries `task_order` (as in v3). Generation v2 took v4 first; any other
/// index change (the composer's fields among them) takes the next free
/// number.
pub const BATCH_INDEX_SCHEMA: &str = "slop_ninja_eval_batch_index_v4";

/// The vendors whose spend is settled under their own ledger keys: the
/// frontier vendors. Workers AI is Cloudflare spend and stays in the
/// cohort's own key with the lanes, as for v1 cohorts.
pub const LEDGER_VENDORS: [Vendor; 2] = [Vendor::Openai, Vendor::Anthropic];

/// A generated document's private generation state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocGen {
    /// The cell the next call uses: the assignment's (after any fallback),
    /// then the rotation's retry cells.
    pub call: Cell,
    pub record: GenRecord,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub model: String,
    pub effort: Effort,
}

/// A cohort's generation context: its plan block, the bound manifest and
/// voice library, and the deployment's rotation seed.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub plan: GenerationPlan,
    pub manifest: Manifest,
    pub manifest_sha256: String,
    pub library: Library,
    pub library_sha256: String,
    pub split: VoiceSplit,
    pub seed: [u8; 32],
}

fn refused(r: slop_ninja_gen_core::manifest::Refused) -> EvalError {
    EvalError::State(r.0)
}

/// The voice library's binding as an object reference.
pub fn library_ref(m: &Manifest) -> ObjectRef {
    ObjectRef { key: m.voice_library.key.clone(), sha256: m.voice_library.sha256.clone(), bytes: m.voice_library.bytes }
}

/// Whether the bound objects' declared sizes (the manifest's alone before
/// it is read, then both) fit [`GEN_OBJECTS_BYTES`]: checked before any
/// read.
pub fn storable(manifest: &ObjectRef, library: Option<&ObjectRef>) -> bool {
    manifest.bytes.saturating_add(library.map_or(0, |l| l.bytes)) <= GEN_OBJECTS_BYTES
}

impl Ctx {
    /// The context from the bound objects' bytes, each checked against its
    /// binding (length and digest) before parsing.
    pub fn from_bytes(plan: &GenerationPlan, manifest: &[u8], library: &[u8], private_seed: &[u8; 32]) -> R<Self> {
        if !storable(&plan.manifest, None) {
            return Err(EvalError::State("generation manifest (larger than the generation objects bound)"));
        }
        let m = Manifest::parse_bound(manifest, &plan.manifest).map_err(refused)?;
        let lref = library_ref(&m);
        if !storable(&plan.manifest, Some(&lref)) {
            return Err(EvalError::State("voice library (with the manifest, larger than the generation objects bound)"));
        }
        let lib = Library::parse_bound(library, &m.voice_library).map_err(refused)?;
        let split = plan.split().ok_or(EvalError::Plan("generation.voice_split"))?;
        Ok(Ctx {
            plan: plan.clone(),
            manifest: m,
            manifest_sha256: plan.manifest.sha256.clone(),
            library: lib,
            library_sha256: lref.sha256,
            split,
            seed: rotation::rotation_seed(private_seed),
        })
    }

    /// Keeps the objects with the cohort (at the draw).
    fn store(sql: &impl Sql, seq: i64, plan: &GenerationPlan, manifest: &[u8], library: &[u8], library_sha256: &str) -> R<()> {
        let m = std::str::from_utf8(manifest).map_err(|_| EvalError::State("generation manifest utf-8"))?;
        let l = std::str::from_utf8(library).map_err(|_| EvalError::State("voice library utf-8"))?;
        for (kind, body, sha) in [("manifest", m, plan.manifest.sha256.as_str()), ("library", l, library_sha256)] {
            sql.exec("INSERT INTO gen_objects (seq, kind, body, sha256) VALUES (?, ?, ?, ?)", &[i(seq), s(kind), s(body), s(sha)])?;
        }
        Ok(())
    }

    /// The context the cohort drew with (`None`: a v1 cohort).
    pub fn load(sql: &impl Sql, c: &Cohort, private_seed: &[u8; 32]) -> R<Option<Self>> {
        let Some(plan) = &c.plan.generation else { return Ok(None) };
        let read = |kind: &str| -> R<Vec<Row>> { Ok(sql.exec("SELECT body FROM gen_objects WHERE seq = ? AND kind = ?", &[i(c.seq), s(kind)])?) };
        let (m, l) = (read("manifest")?, read("library")?);
        let (m, l) = match (m.first(), l.first()) {
            (Some(m), Some(l)) => (m, l),
            _ => return Err(EvalError::State("generation objects (not drawn)")),
        };
        Self::from_bytes(plan, text(m, 0)?.as_bytes(), text(l, 0)?.as_bytes(), private_seed).map(Some)
    }

    fn entry(&self, model: &str) -> R<&ModelEntry> {
        self.manifest.model(model).ok_or(EvalError::State("generation model (not in the manifest)"))
    }

    /// The claim lease of a call on `vendor`: the plan's frontier lease for a
    /// frontier vendor, the generation lanes' lease for Workers AI.
    pub fn lease_ms(&self, vendor: Vendor, wai_lease_ms: u64) -> u64 {
        if vendor.lane() == Lane::Frontier { self.plan.frontier_lanes.lease_ms } else { wai_lease_ms }
    }
}

// ------------------------------------------------------------------- draw

/// The bound objects the Worker read for a draw (checked again here).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawObjects {
    pub manifest: Vec<u8>,
    pub library: Vec<u8>,
}

/// One draw's generation state: the context, the streams it advanced and the
/// vendor states it saw, all saved in the draw's own transaction.
pub struct Draw {
    pub ctx: Ctx,
    streams: BTreeMap<String, StreamState>,
    vendors: VendorStates,
    counts: BTreeMap<String, u64>,
}

fn load_stream(sql: &impl Sql, stream: &str) -> R<StreamState> {
    let rows = sql.exec("SELECT state FROM gen_rotation WHERE stream = ?", &[s(stream)])?;
    match rows.first() {
        Some(r) => serde_json::from_str(text(r, 0)?).map_err(|_| EvalError::State("generation stream state")),
        None => Ok(StreamState::default()),
    }
}

fn save_stream(sql: &impl Sql, stream: &str, st: &StreamState, seq: i64) -> R<()> {
    let j = serde_json::to_string(st).map_err(|_| EvalError::State("generation stream state"))?;
    sql.exec(
        "INSERT INTO gen_rotation (stream, state, updated_seq) VALUES (?, ?, ?)
         ON CONFLICT (stream) DO UPDATE SET state = excluded.state, updated_seq = excluded.updated_seq",
        &[s(stream), s(j), i(seq)],
    )?;
    Ok(())
}

/// The deployment's vendor states (all active before any pause).
pub fn vendor_states(sql: &impl Sql) -> R<VendorStates> {
    let rows = sql.exec("SELECT state FROM gen_vendor_state WHERE id = 1", &[])?;
    match rows.first() {
        Some(r) => serde_json::from_str(text(r, 0)?).map_err(|_| EvalError::State("generation vendor state")),
        None => Ok(VendorStates::default()),
    }
}

fn save_vendor_states(sql: &impl Sql, v: &VendorStates) -> R<()> {
    let j = serde_json::to_string(v).map_err(|_| EvalError::State("generation vendor state"))?;
    sql.exec(
        "INSERT INTO gen_vendor_state (id, state) VALUES (1, ?) ON CONFLICT (id) DO UPDATE SET state = excluded.state",
        &[s(j)],
    )?;
    Ok(())
}

/// The vendor an operator clear names: a frontier vendor (only those
/// pause; Workers AI outages pause the cohort instead).
pub fn pausable_vendor(code: &str) -> Option<Vendor> {
    LEDGER_VENDORS.into_iter().find(|v| v.code() == code)
}

/// An operator clear (`POST /eval/gen/vendor-clear {"vendor"}`; later also
/// an answered probe): `vendor` is active again from the next draw on, and
/// running cohorts' retries may pick it again. Returns whether it was
/// paused.
pub fn resume_vendor(sql: &impl Sql, vendor: Vendor) -> R<bool> {
    let mut v = vendor_states(sql)?;
    let changed = v.resume(vendor);
    if changed {
        save_vendor_states(sql, &v)?;
    }
    Ok(changed)
}

/// A mixed child's span and assignment.
pub struct MixedDraw {
    pub spec: MixedSpec,
    pub record: DocGen,
    /// The voice id (empty: no profile) and operation, for the reuse ledger.
    pub voice: String,
    pub operation: String,
}

impl Draw {
    /// Starts the draw of cohort `seq`: checks and keeps the bound objects.
    pub fn begin(sql: &impl Sql, seq: i64, plan: &GenerationPlan, objects: &DrawObjects, private_seed: &[u8; 32]) -> R<Self> {
        let ctx = Ctx::from_bytes(plan, &objects.manifest, &objects.library, private_seed)?;
        Ctx::store(sql, seq, plan, &objects.manifest, &objects.library, &ctx.library_sha256)?;
        Ok(Draw { ctx, streams: BTreeMap::new(), vendors: vendor_states(sql)?, counts: BTreeMap::new() })
    }

    fn assign(&mut self, sql: &impl Sql, kind: DocKind, domain: &str, key: &str, avoid: &Avoid) -> R<GenAssignment> {
        let stream = rotation::stream_key(self.ctx.manifest.version, kind, domain);
        if !self.streams.contains_key(&stream) {
            let st = load_stream(sql, &stream)?;
            self.streams.insert(stream.clone(), st);
        }
        let st = self.streams.get_mut(&stream).ok_or(EvalError::State("generation stream"))?;
        let vendors = self.vendors;
        let a = rotation::assign(st, &self.ctx.manifest, &self.ctx.library, self.ctx.split, &self.ctx.seed, &stream, kind, key, avoid, &|v| vendors.paused(v))
            .map_err(refused)?;
        *self.counts.entry(format!("lane:{}", a.lane.code())).or_default() += 1;
        if a.fallback.is_some() {
            *self.counts.entry("fallback".into()).or_default() += 1;
        }
        *self.counts.entry(format!("voice:{}", a.voice.category().map_or("none", |c| c.code()))).or_default() += 1;
        Ok(a)
    }

    fn doc(&self, key: &str, a: GenAssignment) -> DocGen {
        let call = Cell { model: a.model.clone(), effort: a.effort };
        DocGen { call, record: GenRecord::new(key, &self.ctx.manifest_sha256, &self.ctx.library_sha256, a) }
    }

    /// A mixed child of `parent_text` (`parent_key` its document key): the
    /// rotation's cell, the span of its span class at the digest's place,
    /// and the parent's earlier models, voices and operations avoided.
    #[allow(clippy::too_many_arguments)]
    pub fn mixed(
        &mut self,
        sql: &impl Sql,
        domain: &str,
        key: &str,
        digest: &[u8; 32],
        parent_key: &str,
        parent_text: &str,
        avoid: Avoid,
    ) -> R<MixedDraw> {
        let a = self.assign(sql, DocKind::Mixed, domain, key, &avoid)?;
        let pct = a.span_percent.ok_or(EvalError::State("generation span class"))?;
        let sents = textgen::sentences(parent_text);
        let (first, last, percent) = textgen::select_span_percent(sents.len(), pct, digest).ok_or(EvalError::State("parent span"))?;
        let start = sents.get(first).ok_or(EvalError::State("span"))?.0;
        let end = sents.get(last).ok_or(EvalError::State("span"))?.1;
        let voice = a.voice.id().unwrap_or("").to_owned();
        let operation = a.operation.clone().unwrap_or_default();
        let match_ctx = a.template == "rewrite_v2_match_context";
        let mut record = self.doc(key, a);
        record.record.parent = Some(parent_key.to_owned());
        if match_ctx {
            record.record.context = Some(((0, start as u64), (end as u64, parent_text.len() as u64)));
        }
        let spec = MixedSpec { first, last, start, end, percent, avoid: avoid.models, voice: None, operation: None };
        Ok(MixedDraw { spec, record, voice, operation })
    }

    /// A model-only document of `domain`: the rotation's cell and length, a
    /// topic of the manifest's register by digest.
    pub fn scratch(&mut self, sql: &impl Sql, domain: &str, key: &str, digest: &[u8; 32]) -> R<(ScratchSpec, DocGen)> {
        let a = self.assign(sql, DocKind::ModelOnly, domain, key, &Avoid::default())?;
        let reg = self.ctx.manifest.register(domain).ok_or(EvalError::State("generation (a pool domain without a register)"))?;
        let n = reg.topics.len().max(1);
        let topic = reg.topics.get(usize::from(digest[2]) % n).ok_or(EvalError::State("generation topic"))?.clone();
        let words = a.scratch_words.ok_or(EvalError::State("generation scratch length"))? as usize;
        Ok((ScratchSpec { topic, words }, self.doc(key, a)))
    }

    /// Saves every advanced stream (in the draw's transaction) and returns
    /// the draw summary's generation block.
    pub fn finish(self, sql: &impl Sql, seq: i64) -> R<Value> {
        for (stream, st) in &self.streams {
            save_stream(sql, stream, st, seq)?;
        }
        Ok(json!({"schema": slop_ninja_gen_core::GENERATION_PLAN_SCHEMA, "rotation": slop_ninja_gen_core::ROTATION_VERSION,
            "manifest_version": self.ctx.manifest.version, "voice_split": self.ctx.split.code(),
            "streams": self.streams.len(), "assigned": self.counts}))
    }
}

pub fn encode(g: &DocGen) -> R<String> {
    serde_json::to_string(g).map_err(|_| EvalError::State("generation record"))
}

fn decode(j: &str) -> R<DocGen> {
    serde_json::from_str(j).map_err(|_| EvalError::State("generation record"))
}

// ------------------------------------------------------------------ claims

/// The document facts a request is built from.
pub struct DocFacts<'a> {
    pub kind: &'a str,
    pub domain: &'a str,
    pub spec: &'a str,
    pub parent_text: Option<&'a str>,
}

/// The vendor request of `g`'s current cell for the document.
pub fn request_for(ctx: &Ctx, g: &DocGen, d: &DocFacts<'_>) -> R<request::VendorRequest> {
    let mut a = g.record.assignment.clone();
    a.model = g.call.model.clone();
    a.effort = g.call.effort;
    let target = match d.kind {
        "mixed" => {
            let spec: MixedSpec = serde_json::from_str(d.spec).map_err(|_| EvalError::State("spec"))?;
            let parent = d.parent_text.ok_or(EvalError::State("parent"))?;
            let context = g.record.context.map(|(b, f)| (b.0 as usize..b.1 as usize, f.0 as usize..f.1 as usize));
            return request::build(&ctx.manifest, &ctx.library, &a, &GenTarget::Rewrite { domain: d.domain, parent, span: spec.start..spec.end, context })
                .map_err(refused);
        }
        _ => serde_json::from_str::<ScratchSpec>(d.spec).map_err(|_| EvalError::State("spec"))?,
    };
    let words = u32::try_from(target.words).map_err(|_| EvalError::State("spec"))?;
    request::build(&ctx.manifest, &ctx.library, &a, &GenTarget::Scratch { domain: d.domain, topic: &target.topic, target_words: words }).map_err(refused)
}

/// The call of `g`'s current cell: its request, the ids the vendor may
/// report, and the call timeout of its lease.
pub fn call_for(ctx: &Ctx, g: &DocGen, d: &DocFacts<'_>, wai_lease_ms: u64) -> R<GenCall> {
    let request = request_for(ctx, g, d)?;
    let entry = ctx.entry(&g.call.model)?;
    let mut accepted_models = vec![entry.id.clone()];
    accepted_models.extend(entry.accepted_aliases.iter().cloned());
    let timeout_ms = crate::eval::lanes::gen_call_timeout_ms(ctx.lease_ms(entry.vendor, wai_lease_ms));
    Ok(GenCall { request, accepted_models, timeout_ms })
}

/// The micro-USD reservation of a claimed call: what the vendor can bill at
/// most for it ([`spend::reservation_nanousd`]).
pub fn reservation_microusd(ctx: &Ctx, call: &GenCall) -> R<u64> {
    let entry = ctx.entry(&call.request.model)?;
    Ok(spend::microusd(spend::reservation_nanousd(entry, call.request.tier, &call.request)))
}

/// A v2 document's stored state, if it has one.
pub fn doc_gen(sql: &impl Sql, seq: i64, idx: i64) -> R<Option<DocGen>> {
    let rows = sql.exec("SELECT gen_record FROM docs WHERE seq = ? AND idx = ?", &[i(seq), i(idx)])?;
    rows.first().and_then(|r| opt_text(r, 0)).map(|j| decode(&j)).transpose()
}

/// The facts and current call of document `idx` (a claim's view).
pub fn claim_call(sql: &impl Sql, ctx: &Ctx, seq: i64, idx: i64, wai_lease_ms: u64) -> R<(DocGen, GenCall)> {
    let rows = sql.exec("SELECT kind, domain, spec, parent, gen_record FROM docs WHERE seq = ? AND idx = ?", &[i(seq), i(idx)])?;
    let r = rows.first().ok_or(EvalError::State("generated doc"))?;
    let g = decode(text(r, 4)?)?;
    let parent = parent_text(sql, seq, r, 3)?;
    let d = DocFacts { kind: text(r, 0)?, domain: text(r, 1)?, spec: text(r, 2)?, parent_text: parent.as_deref() };
    let call = call_for(ctx, &g, &d, wai_lease_ms)?;
    Ok((g, call))
}

fn parent_text(sql: &impl Sql, seq: i64, r: &Row, col: usize) -> R<Option<String>> {
    let Some(p) = r.get(col).and_then(|v| if let SqlValue::Integer(n) = v { Some(*n) } else { None }) else { return Ok(None) };
    let pt = sql.exec("SELECT text FROM docs WHERE seq = ? AND idx = ?", &[i(seq), i(p)])?;
    Ok(Some(text(pt.first().ok_or(EvalError::State("parent"))?, 0)?.to_owned()))
}

// ------------------------------------------------------------------ commits

/// What one committed result did (the commit's counters).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub accepted: u32,
    pub rejected: u32,
    pub outages: u32,
    pub throttled: u32,
    pub fenced: u32,
    pub cost_microusd: u64,
    pub released_microusd: u64,
    /// A Workers AI item reached the plan's consecutive-outage bound.
    pub pause: bool,
    /// The final document's token count finished (`Some(false)`: refused).
    pub count_ok: Option<bool>,
}

fn disabled_cells(sql: &impl Sql, seq: i64) -> R<BTreeSet<(String, String)>> {
    let mut out = BTreeSet::new();
    for r in sql.exec("SELECT model, effort FROM gen_disabled WHERE seq = ?", &[i(seq)])? {
        out.insert((text(&r, 0)?.to_owned(), text(&r, 1)?.to_owned()));
    }
    Ok(out)
}

fn record_spend(sql: &impl Sql, seq: i64, entry: &ModelEntry, effort: Effort, u: &spend::Usage, charge: spend::Charge) -> R<u64> {
    let micro = spend::microusd(charge.cost_nanousd);
    sql.exec(
        "INSERT INTO gen_spend (seq, vendor, model, effort, calls, unmeasured_calls, input_tokens, cached_input_tokens,
            output_tokens, reasoning_tokens, cost_nanousd, cost_microusd) VALUES (?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (seq, vendor, model, effort) DO UPDATE SET calls = calls + 1,
            unmeasured_calls = unmeasured_calls + excluded.unmeasured_calls,
            input_tokens = input_tokens + excluded.input_tokens,
            cached_input_tokens = cached_input_tokens + excluded.cached_input_tokens,
            output_tokens = output_tokens + excluded.output_tokens,
            reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens,
            cost_nanousd = cost_nanousd + excluded.cost_nanousd, cost_microusd = cost_microusd + excluded.cost_microusd",
        &[
            i(seq),
            s(entry.vendor.code()),
            s(&entry.id),
            s(effort.code()),
            i(i64::from(charge.unmeasured)),
            bind_u64(u.input_tokens)?,
            bind_u64(u.cached_input_tokens)?,
            bind_u64(u.output_tokens)?,
            bind_u64(u.reasoning_tokens.unwrap_or(0))?,
            bind_u64(charge.cost_nanousd)?,
            bind_u64(micro)?,
        ],
    )?;
    Ok(micro)
}

fn save_doc(sql: &impl Sql, seq: i64, idx: i64, g: &DocGen) -> R<()> {
    sql.exec("UPDATE docs SET gen_record = ? WHERE seq = ? AND idx = ?", &[s(encode(g)?), i(seq), i(idx)])?;
    Ok(())
}

/// Models already answered with content for this document (the retry's
/// tried set).
fn tried(g: &DocGen) -> Vec<String> {
    g.record.attempts.iter().filter(|a| a.outcome.uses_attempt()).map(|a| a.model_requested.clone()).collect()
}

/// The next cell after `attempt` content answers (or a disabled cell),
/// `extra` models excluded too; unchanged when no other usable cell exists.
/// A paused vendor's models are never chosen: when only they remain, the
/// document moves to Workers AI (as a paused vendor's pending items do).
fn next_cell(ctx: &Ctx, sql: &impl Sql, seq: i64, g: &mut DocGen, key: &str, attempt: u32, extra: &[String]) -> R<()> {
    let disabled = disabled_cells(sql, seq)?;
    let vendors = vendor_states(sql)?;
    let mut tried = tried(g);
    tried.extend(extra.iter().cloned());
    let paused_vendor = |m: &str| ctx.manifest.model(m).and_then(|x| vendors.paused(x.vendor));
    let is_disabled = |m: &str, e: Effort| disabled.contains(&(m.to_owned(), e.code().to_owned())) || paused_vendor(m).is_some();
    match rotation::retry_cell(&ctx.manifest, &ctx.seed, &g.record.assignment, key, attempt, &tried, &is_disabled) {
        Some((model, effort)) => g.call = Cell { model, effort },
        None => {
            // Every usable cell of the frontier lane is a paused vendor's.
            let m = ctx.entry(&g.call.model)?;
            if let Some(cause) = vendors.paused(m.vendor) {
                let from = m.vendor;
                move_to_fallback(ctx, sql, seq, g, from, cause, extra)?;
            }
        }
    }
    Ok(())
}

/// Moves a pending frontier document to Workers AI from its stream's
/// fallback deck (keeping its cell, voice and operation), recorded.
fn move_to_fallback(ctx: &Ctx, sql: &impl Sql, seq: i64, g: &mut DocGen, from: Vendor, cause: FallbackCause, avoid: &[String]) -> R<bool> {
    if g.record.assignment.call_lane() == Lane::WorkersAi {
        return Ok(false);
    }
    let stream = g.record.assignment.stream.clone();
    let mut st = load_stream(sql, &stream)?;
    let av = Avoid { models: avoid.to_vec(), ..Avoid::default() };
    rotation::fallback(&mut st, &ctx.manifest, &ctx.seed, &mut g.record.assignment, &av, from, cause).map_err(refused)?;
    save_stream(sql, &stream, &st, seq)?;
    g.call = Cell { model: g.record.assignment.model.clone(), effort: g.record.assignment.effort };
    Ok(true)
}

/// A paused vendor's pending documents of cohort `seq` move to Workers AI
/// (their claims are fenced: a result in flight records its usage only).
fn fall_back_pending(ctx: &Ctx, sql: &impl Sql, seq: i64, vendor: Vendor, cause: FallbackCause) -> R<u32> {
    let rows = sql.exec("SELECT idx, gen_record, spec, kind FROM docs WHERE seq = ? AND status = 'pending' AND gen_record IS NOT NULL", &[i(seq)])?;
    let mut moved = 0;
    for r in &rows {
        let idx = int(r, 0)?;
        let mut g = decode(text(r, 1)?)?;
        if ctx.entry(&g.call.model)?.vendor != vendor {
            continue;
        }
        let avoid = match (text(r, 3)?, opt_text(r, 2)) {
            ("mixed", Some(spec)) => serde_json::from_str::<MixedSpec>(&spec).map(|x| x.avoid).unwrap_or_default(),
            _ => Vec::new(),
        };
        if move_to_fallback(ctx, sql, seq, &mut g, vendor, cause, &avoid)? {
            moved += 1;
            sql.exec(
                "UPDATE docs SET gen_record = ?, generator_next = ?, claim_gen = claim_gen + 1, claim_until_ms = 0, holder = NULL,
                    infra_failures = 0, not_before_ms = 0 WHERE seq = ? AND idx = ?",
                &[s(encode(&g)?), s(&g.call.model), i(seq), i(idx)],
            )?;
        }
    }
    Ok(moved)
}

/// Every pending document of cohort `seq` whose current cell is `cell` (a
/// disabled cell) moves to its next cell; a claim in flight on it is fenced
/// (its result records usage only).
fn move_off_cell(ctx: &Ctx, sql: &impl Sql, seq: i64, cell: &Cell) -> R<()> {
    let rows = sql.exec(
        "SELECT idx, key, attempts, gen_record FROM docs WHERE seq = ? AND status = 'pending' AND gen_record IS NOT NULL AND generator_next = ?",
        &[i(seq), s(&cell.model)],
    )?;
    for r in &rows {
        let mut g = decode(text(r, 3)?)?;
        if g.call != *cell {
            continue;
        }
        let attempt = u32::try_from(int(r, 2)?).unwrap_or(0).saturating_add(1);
        next_cell(ctx, sql, seq, &mut g, text(r, 1)?, attempt, std::slice::from_ref(&cell.model))?;
        sql.exec(
            "UPDATE docs SET generator_next = ?, gen_record = ?, claim_gen = claim_gen + 1, claim_until_ms = 0, holder = NULL
             WHERE seq = ? AND idx = ?",
            &[s(&g.call.model), s(encode(&g)?), i(seq), i(int(r, 0)?)],
        )?;
    }
    Ok(())
}

/// An accepted answer: final text, model byte span, words, model words and
/// share (ppb).
type Accepted = (String, (usize, usize), usize, usize, u32);

/// Private generation record shards (key, body) and the index block.
pub type SealedRecords = (Vec<(String, Vec<u8>)>, Value);

/// The text a v2 answer becomes, or why it is rejected: the v1 validators
/// (window, preamble, markup, unchanged), the context echo of a
/// match-surrounding rewrite, and the final document's scorer length.
fn validate(sql: &impl Sql, seq: i64, r: &Row, g: &DocGen, out: &str) -> R<Result<Accepted, Reject>> {
    let kind = text(r, 0)?;
    if kind == "mixed" {
        let spec: MixedSpec = serde_json::from_str(text(r, 2)?).map_err(|_| EvalError::State("spec"))?;
        let parent = parent_text(sql, seq, r, 1)?.ok_or(EvalError::State("parent"))?;
        let passage = parent.get(spec.start..spec.end).ok_or(EvalError::State("span"))?;
        let rep = match textgen::validate_rewrite(passage, out) {
            Ok(x) => x,
            Err(e) => return Ok(Err(e)),
        };
        if let Some((b, f)) = g.record.context {
            let before = parent.get(b.0 as usize..b.1 as usize).unwrap_or("");
            let after = parent.get(f.0 as usize..f.1 as usize).unwrap_or("");
            if textgen::context_echo(&[before, after], passage, &rep) {
                return Ok(Err(Reject::ContextEcho));
            }
        }
        let sp = textgen::splice(&parent, (spec.start, spec.end), &rep);
        let share = sp.share_ppb();
        return Ok(Ok((sp.text, sp.span, sp.words_total, sp.words_ai, share)));
    }
    let spec: ScratchSpec = serde_json::from_str(text(r, 2)?).map_err(|_| EvalError::State("spec"))?;
    Ok(textgen::validate_scratch(out, spec.words).map(|t| {
        let w = textgen::words(&t);
        let len = t.len();
        (t, (0, len), w, w, 1_000_000_000)
    }))
}

/// The commit of one v2 result `res` (its document row `r`: kind, parent,
/// spec, attempts, status, infra_failures, claim_gen, reserved_microusd,
/// throttles, throttle_since_ms, key, gen_record): every call is recorded
/// with its charge (a stale claim's too: the spend was real); a current
/// claim then acts on the outcome class.
#[allow(clippy::too_many_arguments)]
pub fn apply_one(
    sql: &impl Sql,
    ctx: &Ctx,
    c: &Cohort,
    max_throttle_ms: u64,
    pause_after: i64,
    r: &Row,
    res: &engine::GenOutcome,
    now_ms: u64,
) -> R<Applied> {
    let seq = c.seq;
    let idx = res.idx;
    let mut out = Applied::default();
    // Each call's claim token is its own: a result already recorded under it
    // is an exact redelivery of a commit (not another call), fenced before
    // anything is recorded or charged again. A stale claim's first result
    // (a call that outlived its lease) is still a call, recorded below.
    if sql
        .exec("INSERT INTO gen_results (seq, idx, claim) VALUES (?, ?, ?) ON CONFLICT DO NOTHING RETURNING claim", &[i(seq), i(idx), i(res.claim)])?
        .is_empty()
    {
        out.fenced = 1;
        return Ok(out);
    }
    let mut g = decode(text(r, 11)?)?;
    let current = text(r, 4)? == "pending" && int(r, 6)? == res.claim;
    // The cell this call ran on is the result's (its claim's cell: model
    // and effort), which a stale result may no longer share with the
    // document's current cell.
    let entry = ctx.entry(&res.generator)?.clone();
    let call_cell = match res.effort {
        Some(effort) => Cell { model: res.generator.clone(), effort },
        None if g.call.model == res.generator => g.call.clone(),
        None => return Err(EvalError::State("generation v2 result without its effort")),
    };
    let facts_parent = parent_text(sql, seq, r, 1)?;
    let facts = DocFacts { kind: text(r, 0)?, domain: text(r, 12)?, spec: text(r, 2)?, parent_text: facts_parent.as_deref() };
    let probe = DocGen { call: call_cell.clone(), record: g.record.clone() };
    let req = request_for(ctx, &probe, &facts).ok();
    let reply = match (&res.reply, &req) {
        (Some(x), _) => (**x).clone(),
        (None, Some(q)) => GenReply::without_answer(OutcomeCode::Outage, q, res.ms),
        (None, None) => return Err(EvalError::State("generation v2 result without a reply")),
    };
    let charge = match &req {
        Some(q) => spend::charge(&entry, q, &reply),
        None if reply.usage_reported => spend::Charge { cost_nanousd: spend::cost_nanousd(&entry, reply.tier, &reply.usage), unmeasured: false },
        None => spend::Charge::default(),
    };
    out.cost_microusd = record_spend(sql, seq, &entry, call_cell.effort, &reply.usage, charge)?;
    if !current {
        out.fenced = 1;
        engine::record_gen_usage(sql, seq, &res.generator, (0, 0, 0, 0, 1), res, out.cost_microusd)?;
        return Ok(out);
    }
    // The live claim ends here: its reservation is replaced by the charge,
    // and its token advances, so a redelivered commit is fenced.
    out.released_microusd = uint(r, 7)?;
    sql.exec(
        "UPDATE docs SET reserved_microusd = 0, holder = NULL, claim_until_ms = 0, claim_gen = claim_gen + 1 WHERE seq = ? AND idx = ?",
        &[i(seq), i(idx)],
    )?;
    let key = text(r, 10)?.to_owned();
    let attempt_no = u32::try_from(g.record.attempts.len()).map_err(|_| EvalError::State("attempts"))?;
    let failures = int(r, 5)? + 1;
    let outages = u32::try_from(failures).unwrap_or(u32::MAX);
    let action = pause::decide(entry.vendor, reply.outcome, outages, u32::try_from(pause_after).unwrap_or(u32::MAX), reply.retry_after_ms);
    // Validate an answer (the final document must also fit the scorer).
    let mut reject: Option<String> = None;
    let mut accepted: Option<Accepted> = None;
    let mut count_refused = None;
    if action == Action::Validate {
        let text_out = reply.text.clone().unwrap_or_default();
        match validate(sql, seq, r, &g, &text_out)? {
            Ok(a) => match crate::eval::v3::scorer_length(&a.0) {
                Ok(true) => {
                    out.count_ok = Some(true);
                    accepted = Some(a);
                }
                Ok(false) => {
                    out.count_ok = Some(true);
                    reject = Some(Reject::TokenLimit.code().to_owned());
                }
                Err(refusal) => {
                    out.count_ok = Some(false);
                    count_refused = Some(refusal);
                }
            },
            Err(e) => reject = Some(e.code().to_owned()),
        }
    } else if action == Action::RetryNextCell {
        reject = Some(reply.outcome.code().to_owned());
    }
    g.record.attempts.push(AttemptRecord::from_reply(&ctx.manifest, attempt_no, entry.vendor, &entry.id, call_cell.effort, &reply, charge, reject.as_deref()));
    let cost = out.cost_microusd;
    let usage_row = |ok, rej, err, thr| engine::record_gen_usage(sql, seq, &res.generator, (ok, rej, err, thr, 0), res, cost);
    if let Some(refusal) = count_refused {
        // A count the arena refused decides nothing about the answer: an
        // outage (no attempt), as for v1.
        refusal.record(sql, seq, now_ms)?;
        out.outages = 1;
        out.pause = failures >= pause_after && entry.vendor == Vendor::WorkersAi;
        sql.exec(
            "UPDATE docs SET infra_failures = ?, not_before_ms = ?, gen_record = ? WHERE seq = ? AND idx = ?",
            &[i(failures), bind_u64(now_ms + engine::gen_backoff_ms(failures))?, s(encode(&g)?), i(seq), i(idx)],
        )?;
        usage_row(0, 0, 1, 0)?;
        return Ok(out);
    }
    match action {
        Action::Validate | Action::RetryNextCell => {
            if let Some((t, (a, b), total, ai, share)) = accepted {
                out.accepted = 1;
                if c.plan.is_v3() && text(r, 0)? == "mixed" {
                    crate::eval::v3::record_child_generator(sql, seq, int(r, 1)?, &entry.id)?;
                }
                g.record.final_ = Some(FinalRecord {
                    attempt: attempt_no,
                    text_sha256: sha_hex(t.as_bytes()),
                    ai_spans: vec![(a as u64, b as u64)],
                    words: total as u64,
                    ai_words: ai as u64,
                    share_ppb: share,
                });
                sql.exec(
                    "UPDATE docs SET status = 'ready', text = ?, text_sha256 = ?, generator = ?, share_ppb = ?,
                        span_start = ?, span_end = ?, words_total = ?, words_ai = ?, attempts = attempts + 1,
                        infra_failures = 0, throttle_since_ms = 0, not_before_ms = 0, gen_record = ? WHERE seq = ? AND idx = ?",
                    &[
                        s(t.clone()),
                        s(sha_hex(t.as_bytes())),
                        s(&entry.id),
                        i(i64::from(share)),
                        i(a as i64),
                        i(b as i64),
                        i(total as i64),
                        i(ai as i64),
                        s(encode(&g)?),
                        i(seq),
                        i(idx),
                    ],
                )?;
                usage_row(1, 0, 0, 0)?;
                return Ok(out);
            }
            // A content answer rejected (or refused, or cut at its limit):
            // the attempt is used and the next cell of the lane answers.
            out.rejected = 1;
            let attempts = int(r, 3)? + 1;
            let status = if attempts >= engine::MAX_GEN_ATTEMPTS { "failed" } else { "pending" };
            next_cell(ctx, sql, seq, &mut g, &key, u32::try_from(attempts).unwrap_or(u32::MAX), &[])?;
            let code = reject.unwrap_or_else(|| reply.outcome.code().to_owned());
            sql.exec(
                "UPDATE docs SET attempts = ?, status = ?, infra_failures = 0, throttle_since_ms = 0, not_before_ms = 0,
                    generator_next = ?, rejects = rejects || ?, gen_record = ? WHERE seq = ? AND idx = ?",
                &[i(attempts), s(status), s(&g.call.model), s(format!("{code};")), s(encode(&g)?), i(seq), i(idx)],
            )?;
            usage_row(0, 1, 0, 0)?;
        }
        Action::Throttle { retry_after_ms } => {
            let (throttles, since) = (int(r, 8)?, int(r, 9)?);
            let since = if since == 0 { now_ms as i64 } else { since };
            if now_ms.saturating_sub(since as u64) <= max_throttle_ms {
                out.throttled = 1;
                let wait = engine::throttle_backoff_ms(idx, throttles).max(retry_after_ms.unwrap_or(0));
                sql.exec(
                    "UPDATE docs SET throttles = ?, throttle_since_ms = ?, not_before_ms = ?, gen_record = ? WHERE seq = ? AND idx = ?",
                    &[i(throttles + 1), i(since), bind_u64(now_ms + wait)?, s(encode(&g)?), i(seq), i(idx)],
                )?;
                usage_row(0, 0, 0, 1)?;
                return Ok(out);
            }
            // Throttled past the plan's bound: an outage.
            outage(ctx, sql, seq, idx, &mut g, &entry, failures, pause_after, &mut out, now_ms, r)?;
            usage_row(0, 0, 1, 0)?;
        }
        Action::Backoff => {
            outage(ctx, sql, seq, idx, &mut g, &entry, failures, pause_after, &mut out, now_ms, r)?;
            usage_row(0, 0, 1, 0)?;
        }
        Action::FallbackOutage => {
            out.outages = 1;
            let avoid = mixed_avoid(r)?;
            move_to_fallback(ctx, sql, seq, &mut g, entry.vendor, FallbackCause::Outage, &avoid)?;
            sql.exec(
                "UPDATE docs SET infra_failures = 0, not_before_ms = 0, generator_next = ?, gen_record = ? WHERE seq = ? AND idx = ?",
                &[s(&g.call.model), s(encode(&g)?), i(seq), i(idx)],
            )?;
            usage_row(0, 0, 1, 0)?;
        }
        Action::PauseVendor(cause) => {
            out.outages = 1;
            let mut v = vendor_states(sql)?;
            if v.pause(entry.vendor, cause, now_ms) {
                save_vendor_states(sql, &v)?;
                // The operator's record (vendor and cause only); the status
                // shows the vendor states.
                engine::log_note(sql, seq, pause_note(entry.vendor, cause), now_ms)?;
            }
            save_doc(sql, seq, idx, &g)?;
            fall_back_pending(ctx, sql, seq, entry.vendor, cause)?;
            usage_row(0, 0, 1, 0)?;
        }
        Action::DisableCell => {
            out.outages = 1;
            sql.exec(
                "INSERT OR IGNORE INTO gen_disabled (seq, model, effort) VALUES (?, ?, ?)",
                &[i(seq), s(&entry.id), s(call_cell.effort.code())],
            )?;
            next_cell(ctx, sql, seq, &mut g, &key, attempt_no.saturating_add(1), std::slice::from_ref(&entry.id))?;
            sql.exec(
                "UPDATE docs SET not_before_ms = 0, generator_next = ?, gen_record = ? WHERE seq = ? AND idx = ?",
                &[s(&g.call.model), s(encode(&g)?), i(seq), i(idx)],
            )?;
            // The cell is disabled for the rest of the cohort: every other
            // pending document on it moves to its next cell now.
            move_off_cell(ctx, sql, seq, &call_cell)?;
            usage_row(0, 0, 1, 0)?;
        }
    }
    Ok(out)
}

/// The fixed alert code of a vendor pause (vendor and cause only).
pub fn pause_note(v: Vendor, cause: FallbackCause) -> &'static str {
    match (v, cause) {
        (Vendor::Openai, FallbackCause::SpendCap) => "gen_vendor_paused:openai:spend_cap",
        (Vendor::Openai, FallbackCause::KeyUnavailable) => "gen_vendor_paused:openai:key_unavailable",
        (Vendor::Anthropic, FallbackCause::SpendCap) => "gen_vendor_paused:anthropic:spend_cap",
        (Vendor::Anthropic, FallbackCause::KeyUnavailable) => "gen_vendor_paused:anthropic:key_unavailable",
        _ => "gen_vendor_paused",
    }
}

fn mixed_avoid(r: &Row) -> R<Vec<String>> {
    Ok(match (text(r, 0)?, opt_text(r, 2)) {
        ("mixed", Some(spec)) => serde_json::from_str::<MixedSpec>(&spec).map(|x| x.avoid).unwrap_or_default(),
        _ => Vec::new(),
    })
}

/// An outage with no attempt used: back off; a Workers AI item reaching the
/// plan's bound pauses the cohort (as for v1), a frontier item never does
/// (it moves to Workers AI at the bound instead, [`Action::FallbackOutage`]).
#[allow(clippy::too_many_arguments)]
fn outage(ctx: &Ctx, sql: &impl Sql, seq: i64, idx: i64, g: &mut DocGen, entry: &ModelEntry, failures: i64, pause_after: i64, out: &mut Applied, now_ms: u64, r: &Row) -> R<()> {
    out.outages = 1;
    if entry.vendor == Vendor::WorkersAi {
        out.pause = failures >= pause_after;
    } else if failures >= pause_after {
        // A frontier item throttled past its bound reaches here, not
        // through `decide`: it moves to Workers AI too.
        let avoid = mixed_avoid(r)?;
        move_to_fallback(ctx, sql, seq, g, entry.vendor, FallbackCause::Outage, &avoid)?;
        sql.exec(
            "UPDATE docs SET infra_failures = 0, not_before_ms = 0, generator_next = ?, gen_record = ? WHERE seq = ? AND idx = ?",
            &[s(&g.call.model), s(encode(g)?), i(seq), i(idx)],
        )?;
        return Ok(());
    }
    sql.exec(
        "UPDATE docs SET infra_failures = ?, not_before_ms = ?, gen_record = ? WHERE seq = ? AND idx = ?",
        &[i(failures), bind_u64(now_ms + engine::gen_backoff_ms(failures))?, s(encode(g)?), i(seq), i(idx)],
    )?;
    Ok(())
}

// --------------------------------------------------------------------- spend

/// A cohort's generation spend by vendor (micro-USD, the per-call charges
/// rounded up as the cohort's `gen_cost_microusd` adds them). A v1 cohort's
/// generation is all Workers AI.
pub fn cost_by_vendor(sql: &impl Sql, c: &Cohort) -> R<BTreeMap<&'static str, u64>> {
    let mut out: BTreeMap<&'static str, u64> = [Vendor::Openai, Vendor::Anthropic, Vendor::WorkersAi].into_iter().map(|v| (v.code(), 0)).collect();
    if c.plan.generation.is_none() {
        out.insert(Vendor::WorkersAi.code(), c.gen_cost_microusd);
        return Ok(out);
    }
    for r in sql.exec("SELECT vendor, SUM(cost_microusd) FROM gen_spend WHERE seq = ? GROUP BY vendor", &[i(c.seq)])? {
        let v = text(&r, 0)?;
        if let Some(slot) = [Vendor::Openai, Vendor::Anthropic, Vendor::WorkersAi].into_iter().find(|x| x.code() == v) {
            out.insert(slot.code(), uint(&r, 1)?);
        }
    }
    Ok(out)
}

/// The per-vendor ledger charges of a finished cohort (`cohort_key` its
/// `<instance>:<label>`): one key per frontier vendor with spend
/// (`gen:<vendor>:<instance>:<label>`), so the monthly ledger and the
/// private spend tracker see vendor spend apart from Cloudflare's.
pub fn vendor_charges(sql: &impl Sql, c: &Cohort, cohort_key: &str) -> R<Vec<(String, u64)>> {
    if c.plan.generation.is_none() {
        return Ok(Vec::new());
    }
    let by = cost_by_vendor(sql, c)?;
    Ok(LEDGER_VENDORS
        .iter()
        .filter_map(|v| by.get(v.code()).copied().filter(|m| *m > 0).map(|m| (spend::ledger_key(*v, cohort_key), m)))
        .collect())
}

/// The operator status block of a v2 cohort (counts only; no text, no
/// model output, no key).
pub fn status(sql: &impl Sql, c: &Cohort) -> R<Value> {
    if c.plan.generation.is_none() {
        return Ok(Value::Null);
    }
    let mut by = serde_json::Map::new();
    for r in sql.exec(
        "SELECT json_extract(gen_record, '$.record.assignment.lane'),
                json_extract(gen_record, '$.record.assignment.fallback') IS NOT NULL,
                json_extract(gen_record, '$.record.assignment.voice.outcome'), status, COUNT(*)
         FROM docs WHERE seq = ? AND gen_record IS NOT NULL GROUP BY 1, 2, 3, 4",
        &[i(c.seq)],
    )? {
        let k = format!("{}{}/{}/{}", text(&r, 0)?, if int(&r, 1)? != 0 { "->workers_ai" } else { "" }, text(&r, 2)?, text(&r, 3)?);
        by.insert(k, json!(int(&r, 4)?));
    }
    let mut cells = Vec::new();
    for r in sql.exec(
        "SELECT vendor, model, effort, calls, unmeasured_calls, input_tokens, output_tokens, reasoning_tokens, cost_microusd
         FROM gen_spend WHERE seq = ? ORDER BY vendor, model, effort",
        &[i(c.seq)],
    )? {
        cells.push(json!({"vendor": text(&r, 0)?, "model": text(&r, 1)?, "effort": text(&r, 2)?, "calls": int(&r, 3)?,
            "unmeasured_calls": int(&r, 4)?, "input_tokens": int(&r, 5)?, "output_tokens": int(&r, 6)?,
            "reasoning_tokens": int(&r, 7)?, "cost_microusd": int(&r, 8)?}));
    }
    let disabled: Vec<Value> = disabled_cells(sql, c.seq)?.into_iter().map(|(m, e)| json!({"model": m, "effort": e})).collect();
    Ok(json!({"lane_voice_status": by, "cells": cells, "disabled_cells": disabled, "vendors": vendor_states(sql)?}))
}

// ---------------------------------------------------------------------- seal

/// The private generation records of a sealed cohort: one line per
/// generated document in `BATCH_SHARD_DOCS`-line shards under
/// `cohorts/<label>/generation/`, and the batch index's `generation` block
/// binding them by digest. `None` for a v1 cohort.
pub fn seal_records(sql: &impl Sql, c: &Cohort) -> R<Option<SealedRecords>> {
    let Some(plan) = &c.plan.generation else { return Ok(None) };
    let rows = sql.exec("SELECT gen_record FROM docs WHERE seq = ? AND gen_record IS NOT NULL ORDER BY idx", &[i(c.seq)])?;
    let mut lines = Vec::with_capacity(rows.len());
    for r in &rows {
        let g = decode(text(r, 0)?)?;
        lines.push(serde_json::to_string(&g.record).map_err(|_| EvalError::State("generation record"))?);
    }
    let sha = |kind: &str| -> R<String> {
        let rows = sql.exec("SELECT sha256 FROM gen_objects WHERE seq = ? AND kind = ?", &[i(c.seq), s(kind)])?;
        Ok(text(rows.first().ok_or(EvalError::State("generation objects (not drawn)"))?, 0)?.to_owned())
    };
    let (manifest_sha, library_sha) = (sha("manifest")?, sha("library")?);
    let mut shards = Vec::new();
    let mut index = Vec::new();
    for (k, chunk) in lines.chunks(engine::BATCH_SHARD_DOCS).enumerate() {
        let body: String = chunk.iter().map(|l| format!("{l}\n")).collect();
        let key = format!("cohorts/{}/generation/shard-{k:04}.jsonl", c.label);
        index.push(json!({"key": key, "docs": chunk.len(), "sha256": sha_hex(body.as_bytes())}));
        shards.push((key, body.into_bytes()));
    }
    let block = json!({"schema": slop_ninja_gen_core::RECORD_SCHEMA, "plan_schema": plan.schema,
        "rotation": plan.rotation, "voice_split": plan.voice_split,
        "manifest": {"key": plan.manifest.key, "sha256": manifest_sha}, "voice_library_sha256": library_sha,
        "documents": lines.len(), "shards": index});
    Ok(Some((shards, block)))
}

/// Whether a generated document's voice draw, kept for the reuse ledger, is
/// a profile.
pub fn voice_code(v: &VoiceChoice) -> &str {
    v.id().unwrap_or("")
}

/// The reuse history of a parent row as v2 avoids it.
pub fn avoid_from(generators: Vec<String>, voices: Vec<String>, operations: Vec<String>) -> Avoid {
    Avoid { models: generators, voices, operations }
}

/// The document digest's use for the span place (as v1).
pub fn digest(seed: &[u8; 32], key: &str) -> [u8; 32] {
    item_digest(seed, key)
}

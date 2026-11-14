//! The paid whole-document check (grants only, release R1a): the quote, the
//! start, the fenced job driver, and how a job's ledger hold ends.
//!
//! **Quote** (`/api/detect/v1/paid/quote {text, seal}`, a signed-in reader):
//! paid checks must be open (`DETECT_PAID = on`, or the account is an
//! operator's `paid_preview` tester); the free check's seal must bind the
//! text under the serving package; the whole text passes the English screen;
//! the sections are planned ([`crate::sections`]) and section 1 must be the
//! seal's free cut, with at least one section past it (`free_cut_only` when
//! every stretch past the cut is listed as not scored). The reader is billed
//! the content tokens past the free cut, at least `credit::PAYG_MIN_TOKENS`,
//! and the ledger's `peek` (which
//! takes one from the account's quote bucket) says what it draws. The quote
//! seal (`slop_ninja_detect_quote_v1`) binds `{text_sha256, sections,
//! paid_tokens, owner_h, free_seal_nonce_h, quote_nonce}`; `quote_nonce` is
//! the hold's idempotency key, so a confirm sent twice places one hold.
//!
//! **Start** (`/api/detect/v1/paid/start {text, seal, quote_seal, expiry}`):
//! the quote seal must be this account's, for this text, and name the free
//! seal sent beside it; `DETECT_STORE` must be on. Then: the start's row
//! (`hold_starts`, keyed by the quote nonce, `open`); the hold; the slug
//! and manage token; the job text `j/<id_h>/text` (the canonical text and
//! its plan, under the page's key with the job AAD) and section 1's result
//! `j/<id_h>/s1` (the free seal's, not scored again); then one batch
//! inserts the pending `private_paid` detection (owned, `owner_via = paid`,
//! holding the free seal's nonce hash, so a seal starts one check) only
//! while the start's row is open, its job (`running`, 1 of k), and deletes
//! the row. The account's `owned/add` records the page.
//!
//! **A start that made no page gives its hold back** behind a fence: one
//! batch sets the row `given_back` and reads whether a job holds the hold,
//! and the hold is released only when none does. After the fence no start
//! of that quote can insert a job, so a concurrent start either made its
//! job first (and the read sees it) or makes none; no job ever runs on a
//! released hold. A row left behind (the hold's answer was lost, the start
//! stopped, or the release failed) names a hold no job holds: the hourly
//! sweep ([`sweep_starts`]) fences it and expires its hold through the
//! ledger after [`START_GRACE_MS`].
//!
//! **The job** (`/api/detect/v1/job {slug}` on the secret host, streamed as
//! [`JobLine`]s): one driver at a time, by a fencing token.
//! - CLAIM sets `lease_token` and `lease_until` on a `running` job whose
//!   lease is free; zero rows means another driver holds it, so this one
//!   streams the progress only.
//! - Per section: RENEW (the lease, and the page still pending and
//!   unexpired), score it (gate class `paid`, keyed by the owner hash; the
//!   channel `paid_page` counts it as an operation, never as pot basis),
//!   put `j/<id_h>/s<n>`, ADVANCE (`sections_done = n` only from `n - 1`).
//! - All done: RENEW, assemble the page's document, put `p/<id_h>`, then
//!   one batch: SCORED (the single point after which no driver can act; only
//!   while the page is still pending and unexpired) and the detection to
//!   `live`. A page that ended since the last RENEW makes the step LOST. Then the job objects go and the ledger
//!   commits; a failed commit leaves the job `scored` for a `commit_hold`
//!   message (and the hourly sweep), and the result shows meanwhile: the
//!   hold reserved the tokens, so nothing can overspend.
//! - LOST (a fenced step matched no row): stop; if a read shows the job or
//!   the page ended, delete what this driver wrote since its last fenced
//!   step (and cancel a job whose page ended); otherwise, a failed read
//!   included, leave them, since the holder or the winner uses the same
//!   keys. Stream the progress.
//! - A section refused for good fails the job (fenced) and its page in one
//!   batch, then releases the hold: "Nothing was charged". No path reaches
//!   `failed` or a release after `scored`.
//! - A transient refusal (busy, restarting, the reader gone, no room, the
//!   invocation's subrequest calls) clears this driver's lease and stops
//!   cleanly: the next open resumes at once.
//!
//! **A hold ends** through [`end_hold`] (the `commit_hold` message, the
//! hourly sweep): a `scored` job commits, a `failed` or `cancelled` one
//! releases (and its job objects go), each once, by the ledger's own
//! idempotency. A commit that finds its hold released (no path above makes
//! one) keeps the page, records `hold_released` on the job and writes the
//! `job {commit_found_released}` point, which raises an alert.
//!
//! **Paused.** `DETECT_PAID` off pauses public jobs at their next open; an
//! operator tester's job (recorded in its job text) runs while it is off.
//!
//! Nothing derived from the text reaches D1 or a metric: the job text and
//! section results are encrypted under the page's own key, as its document
//! is.

use std::cell::Cell;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use slop_ninja_account_wire::credit::{self, PAYG_MIN_TOKENS, PLAN_MIN_TOKENS};
use slop_ninja_account_wire::{
    CommitRequest, ExpireAnswer, ExpireRequest, HoldAnswer, HoldEnd, HoldEndAnswer, HoldRequest, OwnedAddRequest,
    PeekAnswer, PeekRequest, ReleaseRequest,
};
use slop_ninja_detect_page::api::{
    JobLine, QuoteAnswer, QuoteRequest, StartAnswer, StartRequest, Unscored as WireUnscored,
};
use slop_ninja_detect_page::codes::{Band, CutKind, DetState, JobState, Kind, SpansNote};
use slop_ninja_detect_page::ids::IdH;
use slop_ninja_detect_page::internal::Summary;
use slop_ninja_detect_page::page::{DocumentFigures, Section as PageSection, Sections, Span};
use slop_ninja_detect_page::refusal::RefusalCode;
use slop_ninja_detect_page::sn::{Principal, flags};
use slop_ninja_detect_page::{Channel, queue};
use slop_ninja_scorer_tokens::Admit;

use crate::bound::FRAME_TOKENS;
use crate::cut::NoCut;
use crate::db::{Done, Io, Stmt, Val, delete_docs};
use crate::doc::{DOC_V, SecretDoc, Serving, StoredResult};
use crate::keys::{Bytes32, NONCE_BYTES, Pepper, ct_eq, manage_h};
use crate::lookup::{Found, SELECT_SWITCHES, SlugForm, SwitchRows, secret_lookup, slug_form};
use crate::seal::{self, Dated, Opened, Purpose, ResultPayload};
use crate::sections::{self, Plan};
use crate::share::{Keys, draw_secret};
use crate::store::{self, job_prefix, secret_doc_key};

// ------------------------------------------------------------------ objects

/// The job objects' AAD: a job text or section result never opens as a
/// page document, nor the other way round.
pub const JOB_AAD: &[u8] = b"slop_ninja_detect_job_v1";
/// The job objects' version.
pub const JOB_V: u8 = 1;
/// The job text's magic: `SNJ` and the version.
const JOB_MAGIC: &[u8; 4] = b"SNJ1";

/// What the job text holds besides the text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobHeader {
    pub v: u8,
    pub plan: Plan,
    /// Started by an operator's tester while paid checks were closed: the
    /// job runs while `DETECT_PAID` is off.
    pub preview: bool,
}

/// The job text: its header and the canonical text, stored as
/// `SNJ1 ‖ u32 header length ‖ header JSON ‖ text`, so the text is never
/// escaped (the job's memory is one copy per byte, not six).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobText {
    pub header: JobHeader,
    pub text: String,
}

/// A job object that does not parse as its version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadJob;

impl JobText {
    pub fn encode(&self) -> Vec<u8> {
        let header = serde_json::to_vec(&self.header).unwrap_or_default();
        let len = u32::try_from(header.len()).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(8 + header.len() + self.text.len());
        out.extend_from_slice(JOB_MAGIC);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(self.text.as_bytes());
        out
    }

    pub fn decode(mut b: Vec<u8>) -> Result<JobText, BadJob> {
        if b.get(..4) != Some(JOB_MAGIC.as_slice()) {
            return Err(BadJob);
        }
        let len = b.get(4..8).and_then(|l| <[u8; 4]>::try_from(l).ok()).map(u32::from_be_bytes).ok_or(BadJob)?;
        let end = 8usize.checked_add(len as usize).ok_or(BadJob)?;
        let header: JobHeader = serde_json::from_slice(b.get(8..end).ok_or(BadJob)?).map_err(|_| BadJob)?;
        if header.v != JOB_V {
            return Err(BadJob);
        }
        let text = String::from_utf8(b.split_off(end)).map_err(|_| BadJob)?;
        Ok(JobText { header, text })
    }
}

/// One section's result, as the page will print it: offsets are UTF-16
/// offsets into the canonical document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionResult {
    pub v: u8,
    pub index: u32,
    pub band: Band,
    pub confidence_tenths: u16,
    pub confidence_text: String,
    pub share_pct: u8,
    pub share_text: String,
    #[serde(default)]
    pub spans: Vec<Span>,
    /// The section's tokens, frame included.
    pub tokens: u64,
    pub scored_at: u64,
}

impl SectionResult {
    /// Section 1 from the free check's stored result (its offsets are the
    /// canonical text's, which section 1 starts). `None` for a short result,
    /// which has no figures to print in a section.
    pub fn from_free(r: &StoredResult) -> Option<SectionResult> {
        Some(SectionResult {
            v: JOB_V,
            index: 1,
            band: r.band?,
            confidence_tenths: r.confidence_tenths?,
            confidence_text: r.confidence_text.clone()?,
            share_pct: r.share_pct?,
            share_text: r.share_text.clone()?,
            spans: r.spans.clone(),
            tokens: u64::from(r.tokens),
            scored_at: r.scored_at,
        })
    }
}

/// `j/<hex id_h>/text`.
pub fn text_key(id_h: &Bytes32) -> String {
    format!("{}text", job_prefix(id_h))
}

/// `j/<hex id_h>/s<n>`.
pub fn section_key(id_h: &Bytes32, n: u32) -> String {
    format!("{}s{n}", job_prefix(id_h))
}

/// Every job object of a plan of `k` sections.
pub fn job_keys(id_h: &Bytes32, k: u32) -> Vec<String> {
    let mut v = vec![text_key(id_h)];
    v.extend((1..=k).map(|n| section_key(id_h, n)));
    v
}

fn seal_part(pepper: &Pepper, slug: &str, io: &impl Io, plain: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_BYTES];
    io.fill(&mut nonce);
    pepper.doc_key(slug).seal_with_aad(&nonce, JOB_AAD, plain)
}

fn open_part(pepper: &Pepper, slug: &str, sealed: &[u8]) -> Option<Vec<u8>> {
    pepper.doc_key(slug).open_with_aad(JOB_AAD, sealed).ok()
}

/// A quote nonce: 32 lowercase hex digits.
fn nonce_ok(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The pot receipt of a paid document, derived from its hold so a retried
/// commit names the same one: `pr` and 30 hex digits of
/// `SHA-256("paid_receipt_v1" ‖ hold_ref)`.
pub fn receipt_id(hold_ref: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"paid_receipt_v1");
    h.update(hold_ref.as_bytes());
    let d = hex::encode(h.finalize());
    format!("pr{}", d.get(..30).unwrap_or(""))
}

// ------------------------------------------------------------------- seams

/// The account Worker's billing routes (the `ACCOUNT` service binding). A
/// refusal the account answered comes back as its code; a failed call as
/// `unavailable`.
#[allow(async_fn_in_trait)]
pub trait Ledger {
    async fn peek(&self, req: &PeekRequest) -> Result<PeekAnswer, RefusalCode>;
    async fn hold(&self, req: &HoldRequest) -> Result<HoldAnswer, RefusalCode>;
    async fn commit(&self, req: &CommitRequest) -> Result<HoldEndAnswer, RefusalCode>;
    async fn release(&self, req: &ReleaseRequest) -> Result<HoldEndAnswer, RefusalCode>;
    /// The hold of a start this store has fenced ends released, if open.
    async fn expire(&self, req: &ExpireRequest) -> Result<ExpireAnswer, RefusalCode>;
}

/// One section's scan, as the driver needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scored {
    pub band: Band,
    pub confidence_tenths: u16,
    pub confidence_text: String,
    pub share_pct: u8,
    pub share_text: String,
    /// UTF-16 offsets into the section's text.
    pub spans: Vec<Span>,
    pub tokens: u64,
}

/// Why a section was not scored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreFail {
    /// Busy, restarting, no room, the reader gone or the calls spent: stop
    /// and let the next open resume.
    Transient,
    /// Refused for good: the job fails and nothing is charged.
    Refused(RefusalCode),
}

/// The scorer the driver uses: one section through the gate (class paid)
/// and the lane. It spends from `calls` for every call it makes.
#[allow(async_fn_in_trait)]
pub trait SectionScorer {
    async fn score(&self, text: &str, words: u32, tokens: u64, calls: &Calls) -> Result<Scored, ScoreFail>;
}

/// Where the job's lines go.
#[allow(async_fn_in_trait)]
pub trait JobSink {
    /// `false` when the reader is gone.
    async fn line(&mut self, l: &JobLine) -> bool;
    fn alive(&mut self) -> bool;
}

/// The documented Workers limit on subrequests per invocation; every
/// binding call (D1, R2, a Durable Object, a service binding, the queue)
/// counts.
pub const SUBREQUESTS_PER_INVOCATION: u32 = 10_000;
/// The calls one section may take at most: RENEW, the gate's enter and
/// done, the lane, the section's put and ADVANCE (6), and the gate's asks
/// within the rest. A section that would need more stops cleanly and the
/// next open resumes it.
pub const SECTION_SUBREQUESTS: u32 = 64;
/// The calls the end takes beside reading each section's result: RENEW,
/// the document's put, the SCORED batch, the job objects' delete, the
/// commit and its batch, a queue send, with room to spare.
pub const ASSEMBLY_FIXED_SUBREQUESTS: u32 = 16;
/// Reading one stored section result (its size and its bytes).
pub const READ_SUBREQUESTS: u32 = 2;

/// The invocation's subrequest calls.
#[derive(Debug)]
pub struct Calls {
    used: Cell<u32>,
    limit: u32,
}

impl Calls {
    pub fn new(limit: u32) -> Calls {
        Calls { used: Cell::new(0), limit }
    }

    pub fn take(&self, n: u32) {
        self.used.set(self.used.get().saturating_add(n));
    }

    /// Room for `n` more calls.
    pub fn fits(&self, n: u32) -> bool {
        self.used.get().saturating_add(n) <= self.limit
    }

    pub fn used(&self) -> u32 {
        self.used.get()
    }
}

/// The assembly's calls for a plan of `k` sections.
pub fn assembly_subrequests(k: u32) -> u32 {
    ASSEMBLY_FIXED_SUBREQUESTS.saturating_add(READ_SUBREQUESTS.saturating_mul(k))
}

// --------------------------------------------------------------- the quote

/// The quote's seal payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotePayload {
    pub day: u64,
    /// `SHA-256(canon_v1(text))`, hex.
    pub text_sha256: String,
    pub sections: u32,
    pub paid_tokens: u64,
    /// The account's owner hash, hex.
    pub owner_h: String,
    /// The free check's seal nonce hash, hex.
    pub free_seal_nonce_h: String,
    /// The hold's idempotency key: 32 hex digits.
    pub quote_nonce: String,
    pub issued_ms: u64,
}

impl Dated for QuotePayload {
    fn day(&self) -> u64 {
        self.day
    }
}

/// What the paid routes share.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    /// The store secret's bytes (the seals' day keys).
    pub secret: &'a [u8; seal::SECRET_BYTES],
    pub keys: Keys<'a>,
    pub serving: &'a Serving,
    /// The longest canonical text a paid check takes.
    pub max_text_bytes: usize,
    /// The largest section one scan may hold.
    pub max_section_bytes: usize,
}

/// A text that passed the checks the quote and the start share.
struct Checked {
    canonical: String,
    plan: Plan,
    free: Opened<ResultPayload>,
    first: SectionResult,
    /// Started while paid checks are closed, by a tester.
    preview: bool,
}

/// Whether paid checks are open to `p`: to everyone while `DETECT_PAID` is
/// on, and to an operator's `paid_preview` tester (`Ok(true)`) while off.
fn open_to(io: &impl Io, sw: &SwitchRows, p: &Principal) -> Result<bool, RefusalCode> {
    if sw.on(io, "DETECT_PAID") {
        Ok(false)
    } else if p.has(flags::PAID_PREVIEW) {
        Ok(true)
    } else {
        Err(RefusalCode::PaidOff)
    }
}

fn plan_code(e: NoCut) -> RefusalCode {
    match e {
        NoCut::NoSentenceEnd => RefusalCode::NoSentenceEnd,
        NoCut::Count(_) => RefusalCode::Busy,
    }
}

/// The checks the quote and the start share: the free seal for this text
/// and package, the English screen on the whole text, the plan, and
/// section 1 equal to the seal's free cut.
fn check(
    cx: &Ctx<'_>,
    now: u64,
    text: &str,
    seal_s: &str,
    preview: bool,
    admit: Admit<'_>,
) -> Result<Checked, RefusalCode> {
    let free = seal::open_result(cx.secret, now, seal_s, text).map_err(|_| RefusalCode::SealInvalid)?;
    let p = &free.payload;
    let stored = crate::handoff::stored_result(&p.result_full, p.issued_ms, &p.package_load, &p.policy_v, cx.serving)
        .map_err(|_| RefusalCode::SealInvalid)?;
    if stored.cut == CutKind::None {
        // Nothing past the free cut: the free check scored it all.
        return Err(RefusalCode::MalformedRequest);
    }
    let canonical = crate::canon::canon_v1(text).into_owned();
    if canonical.len() > cx.max_text_bytes {
        return Err(RefusalCode::TooLarge);
    }
    if crate::english::english_screen(&canonical).is_some() {
        return Err(RefusalCode::NotEnglish);
    }
    let plan = sections::plan(&canonical, cx.max_section_bytes, admit).map_err(plan_code)?;
    let s1 = plan.sections.first().ok_or(RefusalCode::NoSentenceEnd)?;
    if sections::utf16_at(&canonical, s1.end) != p.scored_end || s1.tokens != u64::from(stored.tokens) {
        return Err(RefusalCode::SealInvalid);
    }
    if plan.sections.len() < 2 {
        // Past the free cut nothing can be scored (every stretch is listed
        // as not scored): there is nothing to sell, so no quote at the
        // minimum for it.
        return Err(if plan.unscored.is_empty() { RefusalCode::MalformedRequest } else { RefusalCode::FreeCutOnly });
    }
    // A free cut too dense in tokens to reach the measured range has no
    // section figures; the whole document cannot be checked.
    let first = SectionResult::from_free(&stored).ok_or(RefusalCode::TooLong)?;
    Ok(Checked { canonical, plan, free, first, preview })
}

/// The billed tokens of a plan.
pub fn paid_tokens_of(plan: &Plan) -> u64 {
    plan.paid_tokens.max(PAYG_MIN_TOKENS)
}

fn u32_sat(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn wire_unscored(plan: &Plan) -> Vec<WireUnscored> {
    plan.unscored.iter().map(|u| WireUnscored { from_word: u.from_word, to_word: u.to_word }).collect()
}

/// `POST /api/detect/v1/paid/quote`.
pub async fn quote(
    io: &impl Io,
    ledger: &impl Ledger,
    cx: &Ctx<'_>,
    principal: Option<Principal>,
    req: &QuoteRequest,
    admit: Admit<'_>,
) -> Result<QuoteAnswer, RefusalCode> {
    let p = principal.ok_or(RefusalCode::SignedOut)?;
    let sw = SwitchRows::from_done(io.run(Stmt::new(SELECT_SWITCHES, Vec::new())).await.ok().as_ref());
    let preview = open_to(io, &sw, &p)?;
    let now = io.now_ms();
    let c = check(cx, now, &req.text, &req.seal, preview, admit)?;
    let tokens = paid_tokens_of(&c.plan);
    let peek = ledger.peek(&PeekRequest { user_id: p.user_id }).await?;
    let draw = credit::draw(tokens, Channel::PaidPage, peek.plan_left, peek.api_left, peek.credit_left);
    let (draw_plan, draw_credit) = match draw {
        Some(d) => (d.plan, d.credit),
        None => {
            let plan = peek.plan_left.min(tokens.max(PLAN_MIN_TOKENS));
            (plan, tokens.saturating_sub(plan))
        }
    };
    let mut nonce = [0u8; 16];
    io.fill(&mut nonce);
    let payload = QuotePayload {
        day: seal::day_of(now),
        text_sha256: hex::encode(crate::canon::canonical_sha256(&c.canonical)),
        sections: u32_sat(c.plan.sections.len() as u64),
        paid_tokens: tokens,
        owner_h: hex::encode(cx.keys.store.owner_h(&p.user_id.0)),
        free_seal_nonce_h: hex::encode(c.free.nonce_h()),
        quote_nonce: hex::encode(nonce),
        issued_ms: now,
    };
    let mut seal_nonce = [0u8; seal::NONCE_BYTES];
    io.fill(&mut seal_nonce);
    let quote_seal =
        seal::seal(cx.secret, Purpose::Quote, payload.day, seal_nonce, &payload).map_err(|_| RefusalCode::Internal)?;
    io.metric("job", &["quoted"]);
    Ok(QuoteAnswer {
        sections: payload.sections,
        words: c.plan.total_words,
        free_words: c.plan.sections.first().map_or(0, |s| s.words),
        paid_tokens: u32_sat(tokens),
        draw_plan: u32_sat(draw_plan),
        draw_credit: u32_sat(draw_credit),
        enough: draw.is_some(),
        quote_seal,
        unscored: wire_unscored(&c.plan),
    })
}

// --------------------------------------------------------------- the start

/// The pending paid page, only while its start's row is open (the fence a
/// give-back or the sweep sets).
pub const INSERT_PAID: &str = "INSERT INTO detections (id_h, pepper_v, kind, platform, state, state_v, \
created_at, updated_at, expires_at, package_load, manage_h, owner_h, owner_via, seal_nonce_h) \
SELECT ?1, ?2, 'private_paid', 'paste', 'pending', 1, ?3, ?3, ?4, ?5, ?6, ?7, 'paid', ?8 \
WHERE EXISTS (SELECT 1 FROM hold_starts WHERE idem = ?9 AND state = 'open') ON CONFLICT DO NOTHING";

/// Its job: only beside the detection this request inserted.
pub const INSERT_JOB: &str = "INSERT INTO jobs (det_id_h, owner_h, hold_ref, package_load, package_pub, policy_v, \
sections_total, sections_done, tokens_billed, cash_tokens, state, created_at, updated_at) \
SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, 'running', ?10, ?10 \
WHERE EXISTS (SELECT 1 FROM detections WHERE id_h = ?1) ON CONFLICT DO NOTHING";

/// Whether a job holds `hold_ref`.
pub const JOB_OF_HOLD: &str = "SELECT 1 AS n FROM jobs WHERE hold_ref = ?1";

/// A start's row before its hold: open, or (a confirm sent again) its time
/// renewed while it is still open.
pub const UPSERT_START: &str = "INSERT INTO hold_starts (idem, state, at) VALUES (?1, 'open', ?2) \
ON CONFLICT (idem) DO UPDATE SET at = excluded.at WHERE hold_starts.state = 'open'";
/// The start's row goes in the batch that inserted its job.
pub const DELETE_START: &str = "DELETE FROM hold_starts WHERE idem = ?1 AND state = 'open' \
AND EXISTS (SELECT 1 FROM jobs WHERE hold_ref = ?2)";
/// The fence: from here on no start makes a job for this quote's hold.
/// `RETURNING` answers whether this call set it.
pub const FENCE_START: &str = "UPDATE hold_starts SET state = 'given_back' WHERE idem = ?1 AND state = 'open' \
RETURNING 1 AS n";
/// Starts left behind, oldest first.
pub const SELECT_LEFT_STARTS: &str = "SELECT idem FROM hold_starts WHERE at <= ?1 ORDER BY at, idem LIMIT ?2";
/// A start's row, once its hold has ended.
pub const DELETE_LEFT_START: &str = "DELETE FROM hold_starts WHERE idem = ?1 AND state = 'given_back'";
/// How long a start's row stays before the sweep ends its hold: an hour, far
/// beyond the seconds a start takes from its hold to its job's batch (a
/// confirm sent again renews it).
pub const START_GRACE_MS: u64 = 3_600_000;

/// `POST /api/detect/v1/paid/start`.
pub async fn start(
    io: &impl Io,
    ledger: &impl Ledger,
    cx: &Ctx<'_>,
    principal: Option<Principal>,
    req: &StartRequest,
    admit: Admit<'_>,
) -> Result<StartAnswer, RefusalCode> {
    let p = principal.ok_or(RefusalCode::SignedOut)?;
    let sw = SwitchRows::from_done(io.run(Stmt::new(SELECT_SWITCHES, Vec::new())).await.ok().as_ref());
    let preview = open_to(io, &sw, &p)?;
    if !sw.on(io, "DETECT_STORE") {
        return Err(RefusalCode::StoreOff);
    }
    let now = io.now_ms();
    let q = seal::open::<QuotePayload>(cx.secret, Purpose::Quote, now, &req.quote_seal)
        .map_err(|_| RefusalCode::QuoteInvalid)?
        .payload;
    let owner_h = cx.keys.store.owner_h(&p.user_id.0);
    let hex_ok = |h: &str, want: &[u8]| hex::decode(h).is_ok_and(|b| ct_eq(&b, want));
    if seal::day_of(q.issued_ms) != q.day || !hex_ok(&q.owner_h, &owner_h) || !nonce_ok(&q.quote_nonce) {
        return Err(RefusalCode::QuoteInvalid);
    }
    let c = check(cx, now, &req.text, &req.seal, preview, admit).map_err(|e| match e {
        // The quote was for this text; a text that no longer checks out is
        // not the one quoted.
        RefusalCode::NotEnglish | RefusalCode::MalformedRequest | RefusalCode::FreeCutOnly => RefusalCode::QuoteInvalid,
        e => e,
    })?;
    let tokens = paid_tokens_of(&c.plan);
    let free_nonce_h = c.free.nonce_h();
    if !hex_ok(&q.text_sha256, &crate::canon::canonical_sha256(&c.canonical))
        || !hex_ok(&q.free_seal_nonce_h, &free_nonce_h)
        || q.sections as usize != c.plan.sections.len()
        || q.paid_tokens != tokens
    {
        return Err(RefusalCode::QuoteInvalid);
    }
    let used = io.run(Stmt::new(crate::share::SEAL_USED, vec![Val::blob(&free_nonce_h)])).await;
    if used.map_err(|_| RefusalCode::SaveUnavailable)?.first().is_some() {
        return Err(RefusalCode::SealUsed);
    }
    // The start's row first: whatever happens to the hold's answer, the
    // sweep can end a hold no job came to hold.
    let idem = q.quote_nonce.clone();
    io.run(Stmt::new(UPSERT_START, vec![Val::text(idem.clone()), Val::int(now)]))
        .await
        .map_err(|_| RefusalCode::SaveUnavailable)?;
    let hold = ledger
        .hold(&HoldRequest { user_id: p.user_id, tokens, channel: Channel::PaidPage, idem: idem.clone() })
        .await?;
    let cash_tokens: u64 = hold.parts.iter().filter(|x| x.cash).map(|x| x.tokens).sum();
    let (slug, manage) = draw_secret(io);
    let pepper = cx.keys.peppers.current();
    let id_h = pepper.id_h(&slug);
    let k = u32_sat(c.plan.sections.len() as u64);
    let keys = job_keys(&id_h, k);
    let job = JobText { header: JobHeader { v: JOB_V, plan: c.plan.clone(), preview: c.preview }, text: c.canonical };
    let first = serde_json::to_vec(&c.first).map_err(|_| RefusalCode::Internal)?;
    let put = async {
        io.doc_put(&text_key(&id_h), seal_part(pepper, &slug, io, &job.encode())).await?;
        io.doc_put(&section_key(&id_h, 1), seal_part(pepper, &slug, io, &first)).await
    };
    if put.await.is_err() {
        give_back(io, ledger, &idem, &hold.hold_id, &keys).await;
        return Err(RefusalCode::SaveUnavailable);
    }
    let expires_at = req.expiry.duration_ms().map(|d| now.saturating_add(d));
    let batch = vec![
        Stmt::new(
            INSERT_PAID,
            vec![
                Val::blob(&id_h),
                Val::int(u64::from(pepper.v)),
                Val::int(now),
                Val::opt_int(expires_at),
                Val::text(cx.serving.load.clone()),
                Val::blob(&manage_h(&manage)),
                Val::blob(&owner_h),
                Val::blob(&free_nonce_h),
                Val::text(idem.clone()),
            ],
        ),
        Stmt::new(
            INSERT_JOB,
            vec![
                Val::blob(&id_h),
                Val::blob(&owner_h),
                Val::text(hold.hold_id.clone()),
                Val::text(cx.serving.load.clone()),
                Val::text(cx.serving.published.clone()),
                Val::text(seal::POLICY_V),
                Val::int(u64::from(k)),
                Val::int(tokens),
                Val::int(cash_tokens),
                Val::int(now),
            ],
        ),
        Stmt::new(DELETE_START, vec![Val::text(idem.clone()), Val::text(hold.hold_id.clone())]),
    ];
    let inserted = match io.batch(batch).await {
        Ok(d) => d.first().is_some_and(|x| x.changes == 1) && d.get(1).is_some_and(|x| x.changes == 1),
        Err(_) => {
            give_back(io, ledger, &idem, &hold.hold_id, &keys).await;
            return Err(RefusalCode::SaveUnavailable);
        }
    };
    if !inserted {
        // Another start of this quote made its page first (its job holds
        // the hold), a link used this seal, or the start was fenced (a
        // give-back of this quote, or the sweep) and its hold has ended.
        return Err(match give_back(io, ledger, &idem, &hold.hold_id, &keys).await {
            GaveBack::Fenced | GaveBack::Unknown => RefusalCode::QuoteInvalid,
            GaveBack::JobHolds | GaveBack::Released => RefusalCode::SealUsed,
        });
    }
    io.metric("job", &["started"]);
    let add = OwnedAddRequest {
        user_id: p.user_id,
        slug: slug.clone(),
        id_h: IdH(id_h),
        summary: Summary {
            kind: Kind::PrivatePaid,
            band: None,
            short: None,
            strong_sections: None,
            total_sections: Some(k),
            words: job.header.plan.total_words,
            created_at: now,
            expires_at,
        },
        is_owner: true,
    };
    // The page is the account's by its owner hash either way; a failed save
    // leaves it out of "my detections" only.
    if io.owned_add(&add).await.is_err() {
        io.metric("job", &["save_failed"]);
    }
    Ok(StartAnswer { slug, manage })
}

/// What a give-back found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GaveBack {
    /// A job holds the hold (another start of this quote made its page).
    JobHolds,
    /// This call fenced the start and released the hold.
    Released,
    /// The start was fenced already (an earlier give-back, or the sweep):
    /// the hold was or will be released.
    Fenced,
    /// The store could not be read: nothing was released (the sweep ends
    /// the hold later).
    Unknown,
}

/// A start that did not make its page: the job objects go, and the hold
/// goes back only when no job holds it. The fence comes first, in the same
/// batch as the read: once the start's row is `given_back`, no start of
/// this quote can insert a job (`INSERT_PAID` needs the row open), so a
/// concurrent start either made its job before the fence (and the read sees
/// it) or makes none. A failed release, or a failed batch, leaves the hold
/// to the sweep, which fences and expires it.
async fn give_back(io: &impl Io, ledger: &impl Ledger, idem: &str, hold_id: &str, keys: &[String]) -> GaveBack {
    let done = io
        .batch(vec![Stmt::new(FENCE_START, vec![Val::text(idem)]), Stmt::new(JOB_OF_HOLD, vec![Val::text(hold_id)])])
        .await;
    let found = match done {
        Ok(d) => {
            let fenced_now = d.first().is_some_and(|x| !x.rows.is_empty());
            if d.get(1).is_some_and(|x| !x.rows.is_empty()) {
                GaveBack::JobHolds
            } else {
                let _ = ledger.release(&ReleaseRequest { hold_id: hold_id.to_owned() }).await;
                if fenced_now { GaveBack::Released } else { GaveBack::Fenced }
            }
        }
        Err(_) => GaveBack::Unknown,
    };
    let _ = delete_docs(io, keys).await;
    found
}

/// The hourly sweep's part for starts left behind: each start row older
/// than [`START_GRACE_MS`] names a hold no job holds (the batch that inserts
/// a job deletes its row). Fence it, ask the ledger to expire its hold, then
/// delete the row; a failed call keeps the row for the next hour. Answers
/// how many holds this run released.
pub async fn sweep_starts(io: &impl Io, ledger: &impl Ledger) -> Result<u64, crate::db::IoError> {
    let now = io.now_ms();
    let due = io
        .run(Stmt::new(SELECT_LEFT_STARTS, vec![Val::int(now.saturating_sub(START_GRACE_MS)), Val::int(SWEEP_JOBS)]))
        .await?;
    let mut released = 0u64;
    let mut first_err = None;
    for idem in due.rows.iter().filter_map(|r| r.text("idem")).map(str::to_owned) {
        if let Err(e) = io.run(Stmt::new(FENCE_START, vec![Val::text(idem.clone())])).await {
            first_err.get_or_insert(e.into());
            continue;
        }
        match ledger.expire(&ExpireRequest { idem: idem.clone() }).await {
            Ok(a) => {
                if a.by_this_call {
                    released += 1;
                }
                if let Err(e) = io.run(Stmt::new(DELETE_LEFT_START, vec![Val::text(idem)])).await {
                    first_err.get_or_insert(e.into());
                }
            }
            Err(_) => {
                first_err.get_or_insert(crate::db::IoError);
            }
        }
    }
    if released > 0 {
        io.metric_n("job", &["hold_expired"], released as f64);
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(released),
    }
}

// ----------------------------------------------------------------- the job

/// The job row as the driver reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobRow {
    pub hold_ref: String,
    pub package_load: String,
    pub package_pub: Option<String>,
    pub sections_total: u32,
    pub sections_done: u32,
    pub paid_tokens: u64,
    pub cash_tokens: u64,
    pub state: JobState,
    pub hold_ended: bool,
}

pub const SELECT_JOB_ROW: &str = "SELECT hold_ref, package_load, package_pub, sections_total, sections_done, \
tokens_billed, cash_tokens, state, hold_ended_at FROM jobs WHERE det_id_h = ?1";

fn job_row(d: Option<&Done>) -> Option<JobRow> {
    let r = d?.first()?;
    Some(JobRow {
        hold_ref: r.text("hold_ref")?.to_owned(),
        package_load: r.text("package_load")?.to_owned(),
        package_pub: r.text("package_pub").map(str::to_owned),
        sections_total: u32::try_from(r.u64("sections_total")?).ok()?,
        sections_done: u32::try_from(r.u64("sections_done")?).ok()?,
        paid_tokens: r.u64("tokens_billed")?,
        cash_tokens: r.u64("cash_tokens")?,
        state: JobState::parse(r.text("state")?)?,
        hold_ended: r.u64("hold_ended_at").is_some(),
    })
}

/// CLAIM: the lease, only when free.
pub const CLAIM: &str = "UPDATE jobs SET lease_token = ?2, lease_until = ?3, updated_at = ?4 \
WHERE det_id_h = ?1 AND state = 'running' AND (lease_until IS NULL OR lease_until < ?4) RETURNING sections_done";
/// RENEW: the lease, while this driver holds it and the page is still
/// pending and unexpired.
pub const RENEW: &str = "UPDATE jobs SET lease_until = ?3 WHERE det_id_h = ?1 AND lease_token = ?2 \
AND state = 'running' AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?1 AND state = 'pending' \
AND (expires_at IS NULL OR expires_at > ?4)) RETURNING 1 AS ok";
/// ADVANCE: section `?3` is done, only from `?3 - 1`.
pub const ADVANCE: &str = "UPDATE jobs SET sections_done = ?3, updated_at = ?4 WHERE det_id_h = ?1 \
AND lease_token = ?2 AND state = 'running' AND sections_done = ?3 - 1 RETURNING 1 AS ok";
/// SCORED: the single point after which no driver can act, only while the
/// page is still pending and unexpired, so a disable, a removal or an
/// expiry that landed after the last RENEW stops the job (nothing is
/// charged) instead of charging for a page that cannot go live.
pub const SCORED: &str = "UPDATE jobs SET state = 'scored', lease_token = NULL, lease_until = NULL, updated_at = ?3 \
WHERE det_id_h = ?1 AND lease_token = ?2 AND state = 'running' AND sections_done = sections_total \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?1 AND state = 'pending' \
AND (expires_at IS NULL OR expires_at > ?3)) RETURNING hold_ref";
/// The page goes live in the SCORED batch, only beside a scored job.
pub const LIVE: &str = "UPDATE detections SET state = 'live', state_v = state_v + 1, updated_at = ?2, \
doc_key = ?3, doc_bytes = ?4 WHERE id_h = ?1 AND state = 'pending' \
AND EXISTS (SELECT 1 FROM jobs WHERE det_id_h = ?1 AND state = 'scored')";
/// FAILED: fenced, as every driver write is.
pub const FAILED: &str = "UPDATE jobs SET state = 'failed', failure = ?3, lease_token = NULL, lease_until = NULL, \
updated_at = ?4 WHERE det_id_h = ?1 AND lease_token = ?2 AND state = 'running' RETURNING hold_ref";
/// The page fails in the FAILED batch, only beside a failed job.
pub const DET_FAILED: &str = "UPDATE detections SET state = 'failed', refusal = ?2, state_v = state_v + 1, \
updated_at = ?3 WHERE id_h = ?1 AND state = 'pending' AND EXISTS (SELECT 1 FROM jobs WHERE det_id_h = ?1 AND state = 'failed')";
/// A clean stop: this driver's lease ends now, so the next open claims at
/// once.
pub const LET_GO: &str = "UPDATE jobs SET lease_token = NULL, lease_until = NULL, updated_at = ?3 \
WHERE det_id_h = ?1 AND lease_token = ?2 AND state = 'running'";
/// CANCEL (the owner, the sweep, a driver that found the page ended):
/// preempts any driver, whose next fenced step then fails.
pub const CANCEL: &str = "UPDATE jobs SET state = 'cancelled', lease_token = NULL, lease_until = NULL, \
updated_at = ?2 WHERE det_id_h = ?1 AND state = 'running' RETURNING hold_ref";
/// A deleted page's job is cancelled in the delete's own batch (guarded on
/// the page being deleted now).
pub const CANCEL_DELETED: &str = "UPDATE jobs SET state = 'cancelled', lease_token = NULL, lease_until = NULL, \
updated_at = ?2 WHERE det_id_h = ?1 AND state = 'running' \
AND (SELECT d.state FROM detections d WHERE d.id_h = ?1) = 'deleted' RETURNING hex(det_id_h) AS id_h";
/// The ledger confirmed the commit.
pub const COMMITTED: &str =
    "UPDATE jobs SET state = 'committed', hold_ended_at = ?2, updated_at = ?2 WHERE det_id_h = ?1 AND state = 'scored'";
/// The commit found the hold released: the job ends committed with the
/// record of it (no debit was made).
pub const COMMITTED_RELEASED: &str = "UPDATE jobs SET state = 'committed', failure = 'hold_released', \
hold_ended_at = ?2, updated_at = ?2 WHERE det_id_h = ?1 AND state = 'scored'";
/// The ledger confirmed a failed or cancelled job's release.
pub const RELEASED: &str = "UPDATE jobs SET hold_ended_at = ?2 WHERE det_id_h = ?1 \
AND state IN ('failed','cancelled') AND hold_ended_at IS NULL";
/// The pot's paid count: once per document, from cash-backed tokens only
/// (grants are served and never counted).
pub const PAID_RECEIPT: &str = "INSERT INTO paid_receipts (receipt_id, hour, channel, package_load, hold_ref, \
tokens, cash, at) SELECT ?1, ?2, 'paid_page', package_load, hold_ref, cash_tokens, 1, ?3 FROM jobs \
WHERE det_id_h = ?4 AND cash_tokens > 0 ON CONFLICT DO NOTHING";
/// The page's state after a lost fenced step.
pub const SELECT_DET_STATE: &str = "SELECT state, expires_at FROM detections WHERE id_h = ?1";

/// A paid page found by its slug, with its job.
#[derive(Clone, Debug)]
pub struct JobOpen<'p> {
    pub found: Found<'p>,
    pub job: Option<JobRow>,
    pub switches: SwitchRows,
}

/// The job route's lookup: a secret `private_paid` page, or a refusal
/// (answered before any stream).
pub async fn open_job<'p>(io: &impl Io, keys: Keys<'p>, slug: &str) -> Result<JobOpen<'p>, RefusalCode> {
    if slug_form(slug) != Some(SlugForm::Secret) {
        return Err(RefusalCode::NotFound);
    }
    let (found, sw) = secret_lookup(io, keys.peppers, slug).await.map_err(|_| RefusalCode::Unavailable)?;
    let found = found.filter(|f| f.row.kind == Kind::PrivatePaid).ok_or(RefusalCode::NotFound)?;
    if let Some(r) = store::ended_refusal(found.row.state_at(io.now_ms())) {
        return Err(r);
    }
    let d = io
        .run(Stmt::new(SELECT_JOB_ROW, vec![Val::blob(&found.row.id_h)]))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    Ok(JobOpen { job: job_row(Some(&d)), found, switches: sw })
}

/// What the driver is given.
pub struct Drive<'a, I: Io, L: Ledger, S: SectionScorer> {
    pub io: &'a I,
    pub ledger: &'a L,
    pub scorer: &'a S,
    pub serving: &'a Serving,
    /// The work lease: one section's scan lease plus one gate hold.
    pub lease_ms: u64,
    pub calls: &'a Calls,
}

/// How a run of the driver ended (for the tests and the metric).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobEnd {
    /// Every section scored and the page is live.
    Done,
    /// The page was already live or failed: nothing to drive.
    Shown,
    /// Another driver holds the lease: progress only.
    Watching,
    /// Stopped cleanly (paused, transient, no calls, reader gone).
    Stopped,
    /// A fenced step matched no row.
    Lost,
    /// A section was refused for good.
    Failed(RefusalCode),
}

impl<I: Io, L: Ledger, S: SectionScorer> Drive<'_, I, L, S> {
    fn take(&self, n: u32) {
        self.calls.take(n);
    }

    async fn run1(&self, s: Stmt) -> Option<Done> {
        self.take(1);
        self.io.run(s).await.ok()
    }

    /// One fenced statement: `true` when it matched its row.
    async fn fenced(&self, s: Stmt) -> Option<bool> {
        self.run1(s).await.map(|d| !d.rows.is_empty() || d.changes > 0)
    }

    async fn let_go(&self, id: &Bytes32, t: &[u8]) {
        let _ = self.run1(Stmt::new(LET_GO, vec![Val::blob(id), Val::blob(t), Val::int(self.io.now_ms())])).await;
    }

    /// Ends a job's hold now (`end_hold`), or leaves it to a `commit_hold`
    /// message when the ledger cannot be reached.
    async fn end_or_queue(&self, id: &Bytes32) {
        self.take(3);
        if end_hold(self.io, self.ledger, id).await.is_err() {
            self.io.metric_n("job", &["commit_retry"], 1.0);
            let _ =
                self.io.enqueue(vec![queue::QueueMessage::new(queue::Job::CommitHold { det_id_h: IdH(*id) })]).await;
        }
    }

    /// Runs the job of `open` for the reader of `sink`.
    pub async fn run(&self, open: &JobOpen<'_>, slug: &str, sink: &mut impl JobSink) -> JobEnd {
        let (io, row) = (self.io, &open.found.row);
        let id = row.id_h;
        let now = io.now_ms();
        let Some(job) = open.job.clone() else {
            let _ = sink.line(&JobLine::Failed { code: RefusalCode::Internal }).await;
            return JobEnd::Shown;
        };
        let progress = JobLine::Progress { section: job.sections_done, of: job.sections_total };
        match (row.state_at(now), job.state) {
            (DetState::Live, _) => {
                let _ = sink.line(&JobLine::Done).await;
                return JobEnd::Shown;
            }
            (DetState::Failed, _) | (_, JobState::Failed | JobState::Cancelled) => {
                let code = row.refusal.unwrap_or(RefusalCode::Scorer);
                let _ = sink.line(&JobLine::Failed { code }).await;
                return JobEnd::Shown;
            }
            (DetState::Pending, JobState::Running) => {}
            _ => {
                let _ = sink.line(&progress).await;
                return JobEnd::Watching;
            }
        }
        let mut t = [0u8; 16];
        io.fill(&mut t);
        let claimed = self
            .run1(Stmt::new(
                CLAIM,
                vec![Val::blob(&id), Val::blob(&t), Val::int(now.saturating_add(self.lease_ms)), Val::int(now)],
            ))
            .await;
        let Some(done_from) = claimed.as_ref().and_then(Done::first).and_then(|r| r.u64("sections_done")) else {
            let _ = sink.line(&progress).await;
            return JobEnd::Watching;
        };
        let done_from = u32::try_from(done_from).unwrap_or(u32::MAX);
        // The job text: a missing one fails the job (this driver holds the
        // lease of a running job); an unreadable store stops cleanly.
        self.take(READ_SUBREQUESTS);
        let jt = match io.doc_get(&text_key(&id)).await {
            Ok(Some(b)) => match open_part(open.found.pepper, slug, &b).map(JobText::decode) {
                Some(Ok(j)) => j,
                _ => return self.fail(&id, &t, RefusalCode::Internal, job.sections_total, sink).await,
            },
            Ok(None) => return self.fail(&id, &t, RefusalCode::Internal, job.sections_total, sink).await,
            Err(_) => {
                self.let_go(&id, &t).await;
                let _ = sink.line(&progress).await;
                return JobEnd::Stopped;
            }
        };
        if !(open.switches.on(io, "DETECT_PAID") || jt.header.preview) {
            // Paused: the switch is off for public jobs.
            self.let_go(&id, &t).await;
            let _ = sink.line(&progress).await;
            return JobEnd::Stopped;
        }
        if job.package_load != self.serving.load {
            // The serving package changed mid-job: its sections would mix
            // two packages.
            return self.fail(&id, &t, RefusalCode::Unavailable, job.sections_total, sink).await;
        }
        let plan = &jt.header.plan;
        let total = job.sections_total;
        if plan.sections.len() != total as usize {
            return self.fail(&id, &t, RefusalCode::Internal, total, sink).await;
        }
        let mut wrote: Vec<String> = Vec::new();
        for n in done_from.saturating_add(1)..=total {
            if !self.calls.fits(SECTION_SUBREQUESTS.saturating_add(assembly_subrequests(total))) || !sink.alive() {
                self.let_go(&id, &t).await;
                let _ = sink.line(&JobLine::Progress { section: n - 1, of: total }).await;
                return JobEnd::Stopped;
            }
            if !self.renew(&id, &t).await {
                return self.lost(&id, &wrote, sink).await;
            }
            let Some(sec) = plan.sections.get(n as usize - 1).copied() else {
                return self.fail(&id, &t, RefusalCode::Internal, total, sink).await;
            };
            let text = jt.text.get(sec.start..sec.end).unwrap_or("");
            let scored = match self.scorer.score(text, sec.words, sec.tokens, self.calls).await {
                Ok(s) => s,
                Err(ScoreFail::Transient) => {
                    self.let_go(&id, &t).await;
                    let _ = sink.line(&JobLine::Progress { section: n - 1, of: total }).await;
                    return JobEnd::Stopped;
                }
                Err(ScoreFail::Refused(code)) => return self.fail(&id, &t, code, total, sink).await,
            };
            let shift = u32_sat(sections::utf16_at(&jt.text, sec.start) as u64);
            let result = SectionResult {
                v: JOB_V,
                index: n,
                band: scored.band,
                confidence_tenths: scored.confidence_tenths,
                confidence_text: scored.confidence_text,
                share_pct: scored.share_pct,
                share_text: scored.share_text,
                spans: scored.spans.iter().map(|s| Span { start: s.start + shift, end: s.end + shift, ..*s }).collect(),
                tokens: scored.tokens,
                scored_at: io.now_ms(),
            };
            let bytes = serde_json::to_vec(&result).unwrap_or_default();
            let key = section_key(&id, n);
            self.take(1);
            if io.doc_put(&key, seal_part(open.found.pepper, slug, io, &bytes)).await.is_err() {
                self.let_go(&id, &t).await;
                let _ = sink.line(&JobLine::Progress { section: n - 1, of: total }).await;
                return JobEnd::Stopped;
            }
            wrote.push(key);
            let adv =
                Stmt::new(ADVANCE, vec![Val::blob(&id), Val::blob(&t), Val::int(u64::from(n)), Val::int(io.now_ms())]);
            match self.fenced(adv).await {
                Some(true) => wrote.clear(),
                Some(false) => return self.lost(&id, &wrote, sink).await,
                None => {
                    self.let_go(&id, &t).await;
                    let _ = sink.line(&JobLine::Progress { section: n - 1, of: total }).await;
                    return JobEnd::Stopped;
                }
            }
            if !sink.line(&JobLine::Running { section: n, of: total }).await {
                self.let_go(&id, &t).await;
                return JobEnd::Stopped;
            }
        }
        self.finish(open, &id, &t, &jt, &job, slug, sink).await
    }

    async fn renew(&self, id: &Bytes32, t: &[u8]) -> bool {
        let now = self.io.now_ms();
        let s = Stmt::new(
            RENEW,
            vec![Val::blob(id), Val::blob(t), Val::int(now.saturating_add(self.lease_ms)), Val::int(now)],
        );
        self.fenced(s).await == Some(true)
    }

    /// LOST: the job or the page moved on without this driver. What it
    /// wrote goes only when a read shows the job or the page ended; when the
    /// read itself fails, nothing is deleted or cancelled (the winner, or
    /// this driver's next open, uses the same keys, and the hourly backstop
    /// removes true orphans).
    async fn lost(&self, id: &Bytes32, wrote: &[String], sink: &mut impl JobSink) -> JobEnd {
        let io = self.io;
        self.take(1);
        let read = io
            .batch(vec![
                Stmt::new(SELECT_JOB_ROW, vec![Val::blob(id)]),
                Stmt::new(SELECT_DET_STATE, vec![Val::blob(id)]),
            ])
            .await;
        let Ok(d) = read else {
            let _ = sink.line(&JobLine::Progress { section: 0, of: 0 }).await;
            io.metric("job", &["lost"]);
            return JobEnd::Lost;
        };
        let d = Some(d);
        let job = d.as_ref().and_then(|d| job_row(d.first()));
        let det = d.as_ref().and_then(|d| d.get(1)).and_then(Done::first).map(|r| {
            let s = r.text("state").and_then(DetState::parse).unwrap_or(DetState::Deleted);
            store::read_state(s, r.u64("expires_at"), io.now_ms())
        });
        let page_ended = !matches!(det, Some(DetState::Pending | DetState::Live));
        let job_ended = job.as_ref().is_none_or(|j| matches!(j.state, JobState::Failed | JobState::Cancelled));
        if page_ended || job_ended {
            self.take(1);
            let _ = delete_docs(io, wrote).await;
            if page_ended && job.as_ref().is_some_and(|j| j.state == JobState::Running) {
                // The page ended under a running job: cancel it and give its
                // hold back.
                let c = self.run1(Stmt::new(CANCEL, vec![Val::blob(id), Val::int(io.now_ms())])).await;
                if c.is_some_and(|c| !c.rows.is_empty()) {
                    self.end_or_queue(id).await;
                }
            }
        }
        let (section, of) = job.map_or((0, 0), |j| (j.sections_done, j.sections_total));
        let _ = sink.line(&JobLine::Progress { section, of }).await;
        io.metric("job", &["lost"]);
        JobEnd::Lost
    }

    /// A section refused for good: the job and its page fail in one batch,
    /// fenced; only then the hold goes back and the job objects go.
    async fn fail(&self, id: &Bytes32, t: &[u8], code: RefusalCode, k: u32, sink: &mut impl JobSink) -> JobEnd {
        let io = self.io;
        let now = io.now_ms();
        self.take(1);
        let done = io
            .batch(vec![
                Stmt::new(FAILED, vec![Val::blob(id), Val::blob(t), Val::text(code.as_str()), Val::int(now)]),
                Stmt::new(DET_FAILED, vec![Val::blob(id), Val::text(code.as_str()), Val::int(now)]),
            ])
            .await;
        match done {
            Ok(d) if d.first().is_some_and(|x| !x.rows.is_empty()) => {
                self.end_or_queue(id).await;
                self.take(1);
                let _ = delete_docs(io, &job_keys(id, k)).await;
                io.metric("job", &["failed"]);
                let _ = sink.line(&JobLine::Failed { code }).await;
                JobEnd::Failed(code)
            }
            Ok(_) => {
                let _ = sink.line(&JobLine::Progress { section: 0, of: k }).await;
                JobEnd::Lost
            }
            Err(_) => {
                self.let_go(id, t).await;
                let _ = sink.line(&JobLine::Progress { section: 0, of: k }).await;
                JobEnd::Stopped
            }
        }
    }

    /// All sections done: assemble, SCORED with the page live, then the
    /// objects go and the hold commits.
    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        open: &JobOpen<'_>,
        id: &Bytes32,
        t: &[u8],
        jt: &JobText,
        job: &JobRow,
        slug: &str,
        sink: &mut impl JobSink,
    ) -> JobEnd {
        let (io, total) = (self.io, job.sections_total);
        let stop = |n: u32| JobLine::Progress { section: n, of: total };
        if !self.renew(id, t).await {
            return self.lost(id, &[], sink).await;
        }
        let mut results = Vec::with_capacity(total as usize);
        for n in 1..=total {
            self.take(READ_SUBREQUESTS);
            match io.doc_get(&section_key(id, n)).await {
                Ok(Some(b)) => match open_part(open.found.pepper, slug, &b)
                    .and_then(|p| serde_json::from_slice::<SectionResult>(&p).ok())
                    .filter(|r| r.index == n && r.v == JOB_V)
                {
                    Some(r) => results.push(r),
                    None => return self.fail(id, t, RefusalCode::Internal, total, sink).await,
                },
                Ok(None) => return self.fail(id, t, RefusalCode::Internal, total, sink).await,
                Err(_) => {
                    self.let_go(id, t).await;
                    let _ = sink.line(&stop(total)).await;
                    return JobEnd::Stopped;
                }
            }
        }
        let doc = match assemble(&jt.text, &jt.header.plan, &results, job, self.serving, io.now_ms()) {
            Some(d) => d,
            None => return self.fail(id, t, RefusalCode::Internal, total, sink).await,
        };
        let mut nonce = [0u8; NONCE_BYTES];
        io.fill(&mut nonce);
        let sealed = open.found.pepper.doc_key(slug).seal(&nonce, &doc.encode());
        let doc_bytes = sealed.len() as u64;
        let key = secret_doc_key(id);
        self.take(1);
        if io.doc_put(&key, sealed).await.is_err() {
            self.let_go(id, t).await;
            let _ = sink.line(&stop(total)).await;
            return JobEnd::Stopped;
        }
        let now = io.now_ms();
        self.take(1);
        let done = io
            .batch(vec![
                Stmt::new(SCORED, vec![Val::blob(id), Val::blob(t), Val::int(now)]),
                Stmt::new(LIVE, vec![Val::blob(id), Val::int(now), Val::text(key.clone()), Val::int(doc_bytes)]),
            ])
            .await;
        match done {
            Ok(d) if d.first().is_some_and(|x| !x.rows.is_empty()) => {}
            Ok(_) => return self.lost(id, std::slice::from_ref(&key), sink).await,
            Err(_) => {
                self.let_go(id, t).await;
                let _ = sink.line(&stop(total)).await;
                return JobEnd::Stopped;
            }
        }
        self.take(1);
        let _ = delete_docs(io, &job_keys(id, total)).await;
        self.end_or_queue(id).await;
        io.metric("job", &["done"]);
        let _ = sink.line(&JobLine::Done).await;
        JobEnd::Done
    }
}

/// The paid page's document from its sections' results (pure). `None` when
/// a result does not match the plan.
pub fn assemble(
    text: &str,
    plan: &Plan,
    results: &[SectionResult],
    job: &JobRow,
    serving: &Serving,
    now: u64,
) -> Option<SecretDoc> {
    if results.len() != plan.sections.len() {
        return None;
    }
    let mut items = Vec::with_capacity(results.len());
    let (mut weighted, mut owned_total) = (0u64, 0u64);
    let mut spans: Vec<Span> = Vec::new();
    for (s, r) in plan.sections.iter().zip(results) {
        let owned = u64::from(s.owned_to_word + 1 - s.owned_from_word);
        weighted += owned * u64::from(r.share_pct);
        owned_total += owned;
        items.push(PageSection {
            index: s.index,
            owned_from_word: s.owned_from_word,
            owned_to_word: s.owned_to_word,
            start: u32_sat(sections::utf16_at(text, s.owned_start) as u64),
            end: u32_sat(sections::utf16_at(text, s.owned_end) as u64),
            band: r.band,
            confidence_tenths: r.confidence_tenths,
            confidence_text: r.confidence_text.clone(),
            share_pct: r.share_pct,
            share_text: r.share_text.clone(),
            spans_count: u32_sat(r.spans.len() as u64),
        });
        spans.extend(r.spans.iter().copied());
    }
    let share = u8::try_from(weighted.checked_div(owned_total).unwrap_or(0).min(100)).unwrap_or(100);
    let share_text = if share < 1 { "below 1%".to_owned() } else { format!("about {share}%") };
    let strong = items.iter().filter(|x| x.band.is_strong()).count() as u32;
    let spans = merge_spans(text, spans);
    let first_tokens = plan.sections.first().map_or(0, |s| s.tokens.saturating_sub(FRAME_TOKENS));
    let spans_note = match (spans.is_empty(), strong) {
        (false, _) => None,
        (true, 0) => Some(SpansNote::StrongBandsOnly),
        (true, _) => Some(SpansNote::Unavailable),
    };
    let mut markers: Vec<String> = Vec::new();
    for m in crate::policy::markers_in(text) {
        if !markers.iter().any(|x| x == m) {
            markers.push(m.to_owned());
        }
    }
    let result_full = StoredResult {
        scored_at: now,
        package_load: job.package_load.clone(),
        package_label: serving.label.clone(),
        package_pub: job.package_pub.clone().unwrap_or_else(|| serving.published.clone()),
        publication_pending: serving.pending,
        scorer_build: serving.scorer_build.clone(),
        policy: seal::POLICY_V.to_owned(),
        evidence: serving.evidence.clone(),
        band: None,
        short: None,
        confidence_tenths: None,
        confidence_text: None,
        share_pct: None,
        share_text: None,
        words: plan.scored_words(),
        total_words: plan.total_words,
        cut: CutKind::None,
        tokens: u32_sat(first_tokens.saturating_add(plan.paid_tokens)),
        length: None,
        spans,
        spans_note,
        markers,
        invisible: crate::policy::has_invisible(text),
        raw: None,
    };
    Some(SecretDoc {
        v: DOC_V,
        text: text.to_owned(),
        result_full,
        source_check: None,
        sections: Some(Sections {
            items,
            document: DocumentFigures {
                share_pct: share,
                share_text,
                strong_sections: strong,
                total_sections: u32_sat(plan.sections.len() as u64),
            },
        }),
    })
}

/// The document's passages: every section's, in order, with passages two
/// sections found in their overlap merged into one (its confidence the
/// higher), each one's words counted in the text.
pub fn merge_spans(text: &str, mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|s| (s.start, s.end));
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(last) if s.start < last.end => {
                last.end = last.end.max(s.end);
                last.confidence = last.confidence.max(s.confidence);
            }
            _ => out.push(s),
        }
    }
    // Words per merged passage, by one walk over the text's UTF-16 offsets.
    let mut bounds: Vec<(usize, usize)> = Vec::with_capacity(out.len());
    let (mut u16_at, mut it) = (0usize, text.char_indices().peekable());
    let mut byte_at = |target: usize| -> usize {
        while u16_at < target {
            match it.next() {
                Some((_, c)) => u16_at += c.len_utf16(),
                None => break,
            }
        }
        it.peek().map_or(text.len(), |(b, _)| *b)
    };
    for s in &out {
        let b0 = byte_at(s.start as usize);
        let b1 = byte_at(s.end as usize);
        bounds.push((b0, b1));
    }
    for (s, (b0, b1)) in out.iter_mut().zip(bounds) {
        s.words = u32_sat(crate::policy::words(text.get(b0..b1).unwrap_or("")) as u64);
    }
    out
}

/// What a paid page's feedback receipt is issued for: its strongest
/// section's band, at the sections' length. `None` for a page without
/// sections (the result itself is the input then).
pub fn receipt_input(doc: &SecretDoc) -> Option<StoredResult> {
    let s = doc.sections.as_ref()?;
    let band = s.items.iter().map(|x| x.band).max_by_key(|b| match b {
        Band::Weak => 0,
        Band::Unclear => 1,
        Band::Strong => 2,
        Band::VeryStrong => 3,
    })?;
    let mut r = doc.result_full.clone();
    r.band = Some(band);
    r.length = Some(slop_ninja_detect_page::codes::LengthBin::Long);
    Some(r)
}

// ------------------------------------------------------- how a hold ends

/// Why a hold's end did not happen now (the caller retries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndLater;

/// Ends the hold of job `id` as its state asks: a `scored` job commits (with
/// its pot receipt when cash backed it), a `failed` or `cancelled` one
/// releases and loses its job objects. A running or ended job needs nothing.
/// Idempotent: the ledger ends a hold once and answers that end again.
pub async fn end_hold(io: &impl Io, ledger: &impl Ledger, id: &Bytes32) -> Result<(), EndLater> {
    let d = io.run(Stmt::new(SELECT_JOB_ROW, vec![Val::blob(id)])).await.map_err(|_| EndLater)?;
    let Some(job) = job_row(Some(&d)) else { return Ok(()) };
    let now = io.now_ms();
    match job.state {
        JobState::Scored => {
            let req = CommitRequest {
                hold_id: job.hold_ref.clone(),
                receipt_id: receipt_id(&job.hold_ref),
                package_pub: job.package_pub.clone().unwrap_or_default(),
            };
            let a = ledger.commit(&req).await.map_err(|_| EndLater)?;
            let found_released = a.ended != HoldEnd::Committed;
            let mut batch = vec![Stmt::new(
                if found_released { COMMITTED_RELEASED } else { COMMITTED },
                vec![Val::blob(id), Val::int(now)],
            )];
            if !found_released {
                batch.push(Stmt::new(
                    PAID_RECEIPT,
                    vec![Val::text(req.receipt_id.clone()), Val::text(hour_of(now)), Val::int(now), Val::blob(id)],
                ));
            }
            io.batch(batch).await.map_err(|_| EndLater)?;
            if found_released {
                // The fence makes this unreachable; if it happens anyway the
                // page stays (the reader has it) and the operator is told.
                io.metric_n("job", &["commit_found_released"], 1.0);
            }
            io.metric("job", &["committed"]);
            Ok(())
        }
        JobState::Failed | JobState::Cancelled if !job.hold_ended => {
            ledger.release(&ReleaseRequest { hold_id: job.hold_ref.clone() }).await.map_err(|_| EndLater)?;
            io.run(Stmt::new(RELEASED, vec![Val::blob(id), Val::int(now)])).await.map_err(|_| EndLater)?;
            let _ = delete_docs(io, &job_keys(id, job.sections_total)).await;
            io.metric("job", &["released"]);
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The `commit_hold` consumer: ends the job's hold, or counts the retry
/// (`job {commit_retry}`, double 1) and asks the queue for one.
pub async fn commit_hold(io: &impl Io, ledger: &impl Ledger, id: &Bytes32) -> Result<(), EndLater> {
    let r = end_hold(io, ledger, id).await;
    if r.is_err() {
        io.metric_n("job", &["commit_retry"], 1.0);
    }
    r
}

/// `YYYY-MM-DDTHH`, the UTC hour (the counters' key).
pub fn hour_of(ms: u64) -> String {
    crate::counters::hour_key(ms)
}

/// Running jobs whose page ended or expired: the sweep cancels them.
pub const SELECT_ENDED_RUNNING: &str = "SELECT hex(j.det_id_h) AS id_h FROM jobs j JOIN detections d \
ON d.id_h = j.det_id_h WHERE j.state = 'running' AND (d.state <> 'pending' \
OR (d.expires_at IS NOT NULL AND d.expires_at <= ?1)) LIMIT ?2";
/// Jobs whose hold has not ended an hour after their end: the sweep asks
/// again.
pub const SELECT_UNENDED: &str = "SELECT hex(det_id_h) AS id_h FROM jobs WHERE (state = 'scored' \
OR (state IN ('failed','cancelled') AND hold_ended_at IS NULL)) AND updated_at <= ?1 LIMIT ?2";
/// Jobs one sweep touches per step.
pub const SWEEP_JOBS: u64 = 50;
/// How long a hold's end may wait for its own message before the sweep
/// sends another.
pub const UNENDED_GRACE_MS: u64 = 3_600_000;

/// The hourly sweep's paid part: cancel running jobs whose page ended or
/// expired, and ask again for every hold whose end is overdue; each goes to
/// the `commit_hold` consumer ([`end_hold`]). Answers how many it sent.
pub async fn sweep(io: &impl Io) -> Result<usize, crate::db::IoError> {
    let now = io.now_ms();
    let ended = io.run(Stmt::new(SELECT_ENDED_RUNNING, vec![Val::int(now), Val::int(SWEEP_JOBS)])).await?;
    let mut ids: Vec<Bytes32> = Vec::new();
    for r in &ended.rows {
        let Some(id) = r.hex32("id_h") else { continue };
        let c = io.run(Stmt::new(CANCEL, vec![Val::blob(&id), Val::int(now)])).await?;
        if !c.rows.is_empty() {
            ids.push(id);
        }
    }
    let due = io
        .run(Stmt::new(SELECT_UNENDED, vec![Val::int(now.saturating_sub(UNENDED_GRACE_MS)), Val::int(SWEEP_JOBS)]))
        .await?;
    // Each overdue end asked for again is a retry of its hold's end.
    if !due.rows.is_empty() {
        io.metric_n("job", &["commit_retry"], due.rows.len() as f64);
    }
    ids.extend(due.rows.iter().filter_map(|r| r.hex32("id_h")));
    ids.sort_unstable();
    ids.dedup();
    let msgs: Vec<_> =
        ids.iter().map(|id| queue::QueueMessage::new(queue::Job::CommitHold { det_id_h: IdH(*id) })).collect();
    let n = msgs.len();
    io.enqueue(msgs).await?;
    Ok(n)
}

/// After a delete batch that carried [`CANCEL_DELETED`]: each cancelled job's
/// hold goes back through the `commit_hold` consumer.
pub async fn after_cancel(io: &impl Io, cancelled: Option<&Done>) {
    let Some(d) = cancelled else { return };
    let msgs: Vec<_> = d
        .rows
        .iter()
        .filter_map(|r| r.hex32("id_h"))
        .map(|id| queue::QueueMessage::new(queue::Job::CommitHold { det_id_h: IdH(id) }))
        .collect();
    if !msgs.is_empty() {
        let _ = io.enqueue(msgs).await;
    }
}

/// The job objects' memory: what the job route holds per byte of the job
/// text at most, beside one section's scan: the object as read and the
/// plaintext it opens to (the store's document read reserves those), the
/// text and the assembled document's JSON (at most six bytes per byte, every
/// character escaped) and that JSON sealed.
pub const JOB_BYTES_PER_TEXT_BYTE: usize = 1 + 6 + 6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_text_round_trips_and_refuses_another_shape() {
        let plan = Plan { sections: vec![], unscored: vec![], total_words: 3, paid_tokens: 0 };
        let jt = JobText { header: JobHeader { v: JOB_V, plan, preview: true }, text: "a b ünï".into() };
        let b = jt.encode();
        assert_eq!(JobText::decode(b.clone()).unwrap(), jt);
        assert_eq!(JobText::decode(b[1..].to_vec()), Err(BadJob));
        let mut short = b.clone();
        short.truncate(10);
        assert_eq!(JobText::decode(short), Err(BadJob));
        let mut bad = b;
        let n = bad.len();
        bad[n - 1] = 0xff;
        assert_eq!(JobText::decode(bad), Err(BadJob), "text that is not UTF-8");
    }

    #[test]
    fn receipts_are_derived_from_the_hold() {
        let a = receipt_id("h0123");
        assert_eq!(a, receipt_id("h0123"));
        assert_ne!(a, receipt_id("h0124"));
        assert_eq!(a.len(), 32);
        assert!(a.starts_with("pr") && a[2..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn overlapping_passages_merge_and_count_their_words() {
        let text = "one two three four five six seven eight nine ten";
        let s = |start: u32, end: u32, c: u8| Span { start, end, words: 0, confidence: Some(c) };
        let m = merge_spans(text, vec![s(8, 23, 90), s(0, 13, 95), s(28, 33, 80)]);
        assert_eq!(m.len(), 2);
        assert_eq!((m[0].start, m[0].end, m[0].confidence, m[0].words), (0, 23, Some(95), 5));
        assert_eq!((m[1].start, m[1].end, m[1].words), (28, 33, 1));
    }

    #[test]
    fn the_calls_counts_and_bounds() {
        let b = Calls::new(10);
        assert!(b.fits(10) && !b.fits(11));
        b.take(4);
        assert!(b.fits(6) && !b.fits(7));
        assert_eq!(b.used(), 4);
        assert_eq!(assembly_subrequests(3), ASSEMBLY_FIXED_SUBREQUESTS + 6);
    }
}

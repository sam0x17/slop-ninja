//! The paid check's routes on Cloudflare (wasm32 only). Every flow is a
//! pure function of `crate::db::Io` and the ledger seam, tested natively
//! (`crate::paid`); this module admits the request, reserves its memory,
//! builds the invocation's store, keys and serving package, and answers.
//!
//! - **Quote and start** (`/api/detect/v1/paid/{quote,start}`, the site
//!   host, with `X-SN-Principal`): the free check's admission (allow-list,
//!   CSRF, principal only where the route table takes one); a declared body
//!   within `routes::PAID_BODY_BYTES`, its footprint reserved from the scan
//!   memory before a byte is read (`routes::paid_footprint`). The ledger is
//!   the account Worker's billing routes through the `ACCOUNT` binding.
//! - **The job** (`/api/detect/v1/job`, the secret host): the lookup answers
//!   any refusal as JSON; then the job text's footprint is reserved
//!   (`routes::job_footprint`) and the answer becomes an NDJSON stream of
//!   job lines while the driver runs. Each section reserves its own scan
//!   (`routes::section_footprint`) and takes a gate ticket of class `paid`
//!   keyed by the page's owner hash, so the gate alternates paid sections
//!   with free scans and holds one per owner at a time. The invocation keeps
//!   a subrequest calls (`paid::SUBREQUESTS_PER_INVOCATION`); a driver that
//!   runs short stops cleanly and the next open resumes.
//!
//! Nothing here writes a console line or echoes input; every answer is
//! `no-store`.

use slop_ninja_detect_page::api::{JobLine, JobRequest, QuoteRequest, StartRequest};
use slop_ninja_detect_page::codes::Band;
use slop_ninja_detect_page::page::Span;
use slop_ninja_detect_page::{Channel, RefusalCode, RouteId, TicketClass, ae};
use worker::worker_sys::web_sys;
use worker::{Env, Request};

use crate::cf::{
    Accepted, Answer, Client, Lines, Quiet, Reserved, Scan, package, point, read_switches, run, store_secret,
    submitted, waits,
};
use crate::cf_routes::{ctx, declared, forwarded, json_body, no_room, ok, refused};
use crate::paid::{self, Calls, Drive, JobEnd, JobSink, ScoreFail, Scored, SectionScorer};
use crate::routes::forwarded::Admission;
use crate::{answer, routes, switches};

/// A quote's or start's body: its declared length within the paid bound,
/// its footprint reserved before a byte is read. Room taken by other
/// requests answers `paid_busy` (`routes::paid_no_room`), not 503.
async fn paid_body<T: serde::de::DeserializeOwned>(req: &mut Request, env: &Env) -> Result<(T, Reserved), RefusalCode> {
    let n = declared(req).ok_or(RefusalCode::LengthRequired)?;
    if n > routes::PAID_BODY_BYTES {
        return Err(RefusalCode::TooLarge);
    }
    let held = Reserved::take(routes::paid_footprint(n)).map_err(routes::paid_no_room)?;
    let bound = usize::try_from(n).map_err(|_| RefusalCode::TooLarge)?;
    Ok((json_body(req, env, bound).await?, held))
}

/// The principal of a paid route (none on a direct call: `signed_out`).
fn principal_of(
    req: &Request,
    env: &Env,
    route: RouteId,
) -> Result<Option<slop_ninja_detect_page::Principal>, RefusalCode> {
    match forwarded(req, env, route)? {
        Admission::Forwarded { principal, .. } => Ok(principal),
        Admission::Direct => Ok(None),
    }
}

/// `POST /api/detect/v1/paid/quote`.
pub(crate) async fn quote(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::PaidQuote;
    let principal = match principal_of(&req, env, route) {
        Ok(p) => p,
        Err(c) => return refused(env, route, c),
    };
    let (body, _held) = match paid_body::<QuoteRequest>(&mut req, env).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let (c, secret) = match (ctx(env), store_secret(env)) {
        (Ok(c), Some(s)) => (c, s),
        (Err(code), _) => return refused(env, route, code),
        (_, None) => return refused(env, route, RefusalCode::NotConfigured),
    };
    let cx = paid::Ctx {
        secret: &secret,
        keys: c.secrets.keys(),
        serving: &c.serving,
        max_text_bytes: routes::PAID_TEXT_BYTES,
        max_section_bytes: routes::MAX_SECTION_BYTES,
    };
    let mut admit = |bytes: usize, _align: usize| bytes <= routes::PAID_SCRATCH_BYTES;
    match paid::quote(&c.io, &c.io, &cx, principal, &body, &mut admit).await {
        Ok(a) => {
            submitted(env, route, "accepted");
            ok(&a)
        }
        Err(code) => refused(env, route, code),
    }
}

/// `POST /api/detect/v1/paid/start`. The website answers 303 to the secret
/// page with the manage token in the fragment.
pub(crate) async fn start(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::PaidStart;
    let principal = match principal_of(&req, env, route) {
        Ok(p) => p,
        Err(c) => return refused(env, route, c),
    };
    let (body, _held) = match paid_body::<StartRequest>(&mut req, env).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let (c, secret) = match (ctx(env), store_secret(env)) {
        (Ok(c), Some(s)) => (c, s),
        (Err(code), _) => return refused(env, route, code),
        (_, None) => return refused(env, route, RefusalCode::NotConfigured),
    };
    let cx = paid::Ctx {
        secret: &secret,
        keys: c.secrets.keys(),
        serving: &c.serving,
        max_text_bytes: routes::PAID_TEXT_BYTES,
        max_section_bytes: routes::MAX_SECTION_BYTES,
    };
    let mut admit = |bytes: usize, _align: usize| bytes <= routes::PAID_SCRATCH_BYTES;
    match paid::start(&c.io, &c.io, &cx, principal, &body, &mut admit).await {
        Ok(a) => {
            submitted(env, route, "accepted");
            ok(&a)
        }
        Err(code) => refused(env, route, code),
    }
}

/// The job's lines on its NDJSON stream.
struct Stream<'a> {
    lines: &'a mut Lines,
}

impl JobSink for Stream<'_> {
    async fn line(&mut self, l: &JobLine) -> bool {
        self.lines.line(l).await
    }

    fn alive(&mut self) -> bool {
        self.lines.alive()
    }
}

/// One section through the free check's own scan path: a gate ticket of
/// class `paid` keyed by the page's owner hash, the lane and the answer's
/// allow-list, counted on channel `paid_page` (an operation, never pot
/// basis). No feedback receipt is issued per section.
struct CfSections<'a> {
    env: &'a Env,
    /// The page's owner hash (hex), the gate's client key input.
    owner: String,
}

/// A refusal the next open may not meet again.
fn transient(code: RefusalCode) -> bool {
    matches!(
        code,
        RefusalCode::Busy
            | RefusalCode::Unavailable
            | RefusalCode::Restarting
            | RefusalCode::ExpiredPlace
            | RefusalCode::ToolOff
    )
}

/// A section's result line as the driver keeps it; `None` for a line with
/// no figures (a short result, which the plan never scores).
fn scored_of(line: &answer::Answer) -> Option<Scored> {
    let spans = line
        .spans
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|s| {
            Some(Span {
                start: u32::try_from(s.start).ok()?,
                end: u32::try_from(s.end).ok()?,
                words: u32::try_from(s.words).ok()?,
                confidence: s.confidence.map(|c| u8::try_from(c.min(100)).unwrap_or(100)),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Scored {
        band: Band::parse(line.band)?,
        confidence_tenths: line.confidence_tenths?,
        confidence_text: line.confidence_text.clone()?,
        share_pct: line.share_pct?,
        share_text: line.share_text.clone()?,
        spans,
        tokens: line.tokens,
    })
}

impl SectionScorer for CfSections<'_> {
    async fn score(&self, text: &str, words: u32, tokens: u64, calls: &Calls) -> Result<Scored, ScoreFail> {
        let env = self.env;
        let w = waits(env);
        calls.take(1);
        let sw = read_switches(env, switches::SCORE, w.hold_ms).await;
        if !sw.on(switches::TOOL) {
            return Err(ScoreFail::Transient);
        }
        let _held = Reserved::take(routes::section_footprint(text.len())).map_err(|_| ScoreFail::Transient)?;
        let pkg = package(env).ok_or(ScoreFail::Refused(RefusalCode::NotConfigured))?;
        let a = Accepted::section(pkg, text, words as usize, tokens, sw.on(switches::SPANS), Channel::PaidPage)
            .ok_or(ScoreFail::Refused(RefusalCode::TooLong))?;
        let client = Client {
            kind: "owner",
            value: self.owner.clone(),
            class: TicketClass::Paid,
            receipt: false,
            calls: Some(calls),
        };
        match run(env, &client, &mut Quiet, &w, &a).await {
            Scan::Result(line) => scored_of(&line).ok_or(ScoreFail::Refused(RefusalCode::Scorer)),
            Scan::Refused(r) if transient(r.code) => Err(ScoreFail::Transient),
            Scan::Refused(r) => Err(ScoreFail::Refused(r.code)),
            Scan::Gone => Err(ScoreFail::Transient),
        }
    }
}

fn end_code(e: JobEnd) -> &'static str {
    match e {
        JobEnd::Done => "done",
        JobEnd::Shown => "shown",
        JobEnd::Watching => "watching",
        JobEnd::Stopped => "stopped",
        JobEnd::Lost => "lost",
        JobEnd::Failed(_) => "failed",
    }
}

/// `POST /api/detect/v1/job` (the secret host): drives the paid job behind
/// the slug and streams its progress.
pub(crate) async fn job(mut req: Request, env: Env, answer: &Answer) {
    let route = RouteId::Job;
    if let Err(c) = forwarded(&req, &env, route) {
        return answer.send(refused(&env, route, c));
    }
    let body = match json_body::<JobRequest>(&mut req, &env, routes::SMALL_BODY_BYTES).await {
        Ok(b) => b,
        Err(c) => return answer.send(refused(&env, route, c)),
    };
    let c = match ctx(&env) {
        Ok(c) => c,
        Err(code) => return answer.send(refused(&env, route, code)),
    };
    let open = match paid::open_job(&c.io, c.secrets.keys(), &body.slug).await {
        Ok(o) => o,
        Err(code) => return answer.send(refused(&env, route, code)),
    };
    let calls = Calls::new(paid::SUBREQUESTS_PER_INVOCATION);
    // The lookup batch, the job row and the job text's size.
    calls.take(3);
    let size = c.io.doc_size(&paid::text_key(&open.found.row.id_h)).await.unwrap_or(0);
    let _held = match Reserved::take(routes::job_footprint(size)) {
        Ok(h) => h,
        Err(e) => return answer.send(refused(&env, route, no_room(e))),
    };
    let w = waits(&env);
    let Some(mut lines) = Lines::open(answer, w.hold_ms) else { return };
    submitted(&env, route, "accepted");
    let owner = open.found.row.owner_h.map(hex::encode).unwrap_or_default();
    let scorer = CfSections { env: &env, owner };
    let d = Drive {
        io: &c.io,
        ledger: &c.io,
        scorer: &scorer,
        serving: &c.serving,
        lease_ms: w.scan_lease_ms().saturating_add(w.hold_ms),
        calls: &calls,
    };
    let mut sink = Stream { lines: &mut lines };
    let end = d.run(&open, &body.slug, &mut sink).await;
    point(&env, ae::detect::JOB, &[end_code(end)], f64::from(calls.used()));
    lines.close().await;
}

//! The public URL check (`POST /api/detect/v1/url {url}`, release R2).
//!
//! Two phases. [`admit`] runs everything that can refuse before a slug
//! exists, so a refusal there stores nothing and draws no slug; it ends in a
//! page to show ([`Admitted::Done`]), a check another invocation drives
//! ([`Admitted::Joined`]), or a claimed check this invocation drives
//! ([`Admitted::Drive`]), which [`drive`] runs to its end.
//!
//! **A web page or a Bluesky post:**
//! 1. the URL rules (`detect-url::admit`: the input checks, `canon_url_v1`,
//!    `capability_url_v1`, the platform rule); a `slop.ninja/d/<slug>` link
//!    answers that slug;
//! 2. `DETECT_TOOL` and `DETECT_URLS`; the host's opt-out row;
//! 3. the security screen through `DetectHost` (`security_risk`);
//! 4. the link's source under its alias: a live page under the serving
//!    package answers its slug (no fetch, no scan); else a page of the
//!    source under a legal state (`removed`, `excerpt_withheld`, `disabled`,
//!    under any package) answers its slug, and an `opted_out` source answers
//!    `rerun_blocked`, so sending the link again never undoes a removal. A
//!    legal state blocks new pages of its source until it is lifted
//!    (`restore`) or its page is deleted; a page that already existed (an
//!    alias, a page under another package) never lifts it. A recent `robots`
//!    or `private_source` failure answers its failed page for the robots
//!    cache period;
//! 5. the claim ([`crate::claims`]): the winner draws the slug;
//! 6. (`drive`) a politeness slot from `DetectHost` (robots.txt first when
//!    stale; `robots` when it disallows), then `DETECT_FETCH /fetch` (once
//!    more when the call itself fails), every hop re-checked there;
//! 7. a failed fetch ends the page `failed` with the fetch's code, keeping no
//!    text; a source that asks not to be indexed marks a known source
//!    `unlisted`, and a 404 or 410 marks it `gone` (an `opted_out` source
//!    stays so). The text's site is the final URL's (a redirect may lead to
//!    another site; the fetch Worker screened it and read its robots.txt):
//!    when its host is not the submitted one, its opt-out row ends the page
//!    `failed` (`private_source`), keeping no text;
//! 8. reputation (ranking shards and the family screen) of that site, which
//!    is also the host the page and its card name;
//! 9. the source is the final URL's (else the submitted link's), and the
//!    link's forms are its aliases. Canonical dedupe: when that source, or
//!    the declared `rel=canonical` on the text's registrable domain, already
//!    has a live page under this package, this page becomes an alias of
//!    that one (same run, `rel=canonical` to it) and answers its slug. The
//!    declared canonical only ever joins such a page: it never keys, names
//!    or aliases a new source, since anyone who can publish on a shared
//!    domain could declare another author's page as theirs. A source under
//!    a legal state ends the page `failed` (`rerun_blocked`);
//! 10. `canon_v1`, then `content_key` reuse (public text only), else the
//!     scorer (the floor, the English screen, the free cut, a gate ticket of
//!     class `url`);
//! 11. storage: the run document `r/<run_id>` (passages, anchors, offsets;
//!     never the full text) first, then one D1 batch fenced by the claim's
//!     token: the source, its aliases, the run, the page `live` with its
//!     `sitemap_seq`, older pages of the source superseded, the shards marked
//!     dirty, the claim deleted; an IndexNow ping is queued. The page goes
//!     live only if, inside that batch, neither its source nor the link's
//!     is `opted_out` or under a legal state; otherwise the same batch ends
//!     it `failed` (`rerun_blocked`) and deletes its new run, and the run
//!     document goes after it. So a removal that lands while the check runs
//!     still wins.
//!
//! **A post on X** (rule `x_post_v1`; no `DetectHost`, robots, Radar or
//! security screen: oEmbed is an API endpoint, not a crawl):
//! 1. `DETECT_URLS` and `DETECT_X` on, else `x_off`;
//! 2. rule `x_age_v1`: a post inside its edit window answers
//!    `x_post_too_new {after_ms}` before any request;
//! 3. the removal tombstone answers `x_removed` with no request; a live page
//!    under the serving package answers its slug;
//! 4. the claim; then `DETECT_FETCH /x/post`: a refusal after the claim
//!    (`x_post_long`, `x_post_unavailable`, `x_busy`, `x_unavailable`,
//!    `too_short`, `not_english`) ends the page `failed` with no source, and
//!    keeps nothing about the post; an existing source the embed no longer
//!    shows gets `fetched_at = 0`, so the next sweep checks and deletes it;
//! 5. the text is scored like any URL text, and the run document holds the
//!    document-level result only; the source keeps the post's canonical URL,
//!    `reputation = 'ranked'`, `fetched_at` now. No sitemap, no IndexNow;
//! 6. the page goes live only on the post's live source: a removal
//!    (the form, `remove` or `x_remove`) that left the tombstone while the
//!    check ran makes the same batch end the page `failed` (`x_removed`)
//!    with no source and delete its new run, and the run document goes
//!    after it. A removal writes the tombstone before it reads the post's
//!    pages and runs (`cases::x_removal`), so a check either stored
//!    before that write, and the removal deletes it, or is fenced off here.
//!    Nothing of the post outlives the removal.

use slop_ninja_detect_fetch_wire::{FetchAnswer, FetchRequest, Limits, MAX_HOPS, Outcome, XPostAnswer, XPostRequest};
use slop_ninja_detect_page::codes::{DetectXState, Platform, Reputation};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::queue::{Job, QueueMessage};
use slop_ninja_detect_page::refusal::{ApiRefusal, RefusalCode};
use slop_ninja_detect_page::slug::{self as pslug, PUBLIC_LEN};
use slop_ninja_detect_page::x::{X_HOST, x_canonical};
use slop_ninja_detect_url::{Admitted as UrlAdmitted, Policy, Refusal, platform};

use crate::claims::{self, Claim, FENCE, Held};
use crate::db::{Done, Io, IoError, Stmt, Val};
use crate::doc::{RunDoc, Serving, StoredResult};
use crate::excerpt::{self, SourceKind};
use crate::keys::{Bytes32, StoreKeys, alias_key, source_canon_key};
use crate::lookup::{SELECT_SWITCHES, SwitchRows};
use crate::net::{FetchApi, HostApi, Screen, Slot};
use crate::sitemap::SHARD_URLS;
use crate::store::run_doc_key;
use crate::switches;
use crate::x_flow;
use crate::xpost;

/// The canonical form's version (`runs.canon_v`).
pub const CANON_V: &str = "canon_v1";
/// The X text rule (`runs.extract_v` of an X run).
pub const X_TEXT_V: &str = "x_text_v1";
/// How long a `robots` or `private_source` failure answers for its link:
/// the robots cache period (RFC 9309 section 2.4).
pub const REFUSAL_HOLD_MS: u64 = 24 * 3_600_000;
/// A source whose response states no freshness is fresh this long.
pub const DEFAULT_FRESH_MS: u64 = 24 * 3_600_000;

/// What the flow needs besides the store and the services.
#[derive(Clone, Debug)]
pub struct UrlCtx<'a> {
    pub serving: &'a Serving,
    pub origins: &'a Origins,
    pub keys: &'a StoreKeys,
    pub policy: &'a Policy,
    /// The fetch wait (`FETCH_WAIT_MS`): every fetch and X request's deadline.
    pub fetch_ms: u64,
    /// The claim's lease: the fetch wait, plus the scan lease, plus one hold.
    pub lease_ms: u64,
    /// The largest stripped HTML the fetch Worker may hold (its own bound
    /// clamps this further).
    pub html_bound: u64,
}

/// The scored part of a URL text.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    /// The result, passage offsets UTF-16 into `scored`.
    pub result: StoredResult,
    /// The scored part (the free cut of the canonical text).
    pub scored: String,
}

/// Scores a canonical URL text under the serving package (gate class
/// `url`, counted as `free_url`).
#[allow(async_fn_in_trait)]
pub trait UrlScorer {
    async fn score(&self, canonical: &str) -> Result<Scored, ApiRefusal>;
}

/// What a link reads as (held once per check, inside a boxed [`Drive`]).
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Link {
    /// A web page or a Bluesky post.
    Page { target: slop_ninja_detect_url::Target, submitted_key: String },
    /// A post on X.
    XPost { id: u64 },
}

/// A claimed check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    pub held: Held,
    pub link: Link,
}

/// How [`admit`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// A page to show now.
    Done(String),
    /// Another invocation drives this check: wait on its page.
    Joined(String),
    /// This invocation drives the check.
    Drive(Box<Drive>),
}

/// How [`drive`] ended: the page's slug, and its failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub slug: String,
    pub failed: Option<RefusalCode>,
}

fn refusal(code: RefusalCode) -> ApiRefusal {
    ApiRefusal::new(code)
}

/// The URL rules' refusal as the pages' code.
pub fn url_refusal(r: Refusal) -> RefusalCode {
    RefusalCode::parse(r.public_code()).unwrap_or(RefusalCode::BadUrl)
}

/// The condition on a page `b` of source `s` that stops any new public page
/// of the source: a legal state (`removed`, `excerpt_withheld`,
/// `disabled`), under any package. It holds until the state is lifted or
/// the page deleted; no other page of the source lifts it (an alias page or
/// a page under another package is older than the block, since the block
/// stops every new one).
macro_rules! blocking_page {
    () => {
        "b.source_id = s.source_id AND b.kind = 'public_url' \
AND b.state IN ('removed','excerpt_withheld','disabled')"
    };
}

/// The same condition as a constant: every statement that checks it
/// carries this text.
pub const BLOCKING_PAGE: &str = blocking_page!();

/// The sources of a key, by its own key or an alias (at most two rows),
/// each with its newest live page under `package` and its newest blocking
/// page under any package ([`BLOCKING_PAGE`]).
pub const SELECT_KNOWN: &str = concat!(
    "SELECT s.source_id AS source_id, s.state AS sstate, s.canon_url AS canon_url, \
(SELECT d.public_slug FROM detections d WHERE d.source_id = s.source_id AND d.kind = 'public_url' \
AND d.state = 'live' AND d.package_load = ?3 AND d.superseded_by IS NULL ORDER BY d.created_at DESC LIMIT 1) \
AS live_slug, (SELECT b.public_slug FROM detections b WHERE ",
    blocking_page!(),
    " ORDER BY b.created_at DESC LIMIT 1) AS blocked_slug FROM sources s WHERE s.canon_key = ?1 \
OR s.source_id = (SELECT a.source_id FROM source_aliases a WHERE a.alias_key = ?2)"
);

/// What [`SELECT_KNOWN`] found for a link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Known {
    /// A live page under the serving package.
    live: Option<String>,
    /// A page under a legal state ([`BLOCKING_PAGE`]).
    blocked: Option<String>,
    /// A source that is `opted_out`.
    opted_out: bool,
}

impl Known {
    fn of(d: Option<&Done>) -> Known {
        let rows = d.map(|d| d.rows.as_slice()).unwrap_or(&[]);
        Known {
            live: rows.iter().find_map(|r| r.text("live_slug")).map(str::to_owned),
            blocked: rows.iter().find_map(|r| r.text("blocked_slug")).map(str::to_owned),
            opted_out: rows.iter().any(|r| r.text("sstate") == Some("opted_out")),
        }
    }

    fn stmt(key: &str, package_load: &str) -> Stmt {
        Stmt::new(
            SELECT_KNOWN,
            vec![Val::blob(&source_canon_key(key)), Val::blob(&alias_key(key)), Val::text(package_load)],
        )
    }
}

/// A recent failure that holds for its link (`robots`, `private_source`),
/// by the claim key the failed page was drawn under (`refusal_marks`: a
/// hash and a page id, nothing of the link).
pub const SELECT_HELD_FAILURE: &str = "SELECT d.public_slug AS public_slug FROM refusal_marks f \
JOIN detections d ON d.id_h = f.det_id_h WHERE f.claim_key = ?1 AND d.state = 'failed' \
AND d.refusal IN ('robots','private_source') AND d.updated_at > ?2";

/// The host's opt-out row.
pub const SELECT_OPTOUT: &str = "SELECT 1 AS hit FROM optouts WHERE host = ?1";

/// Every step before a slug exists.
pub async fn admit(io: &impl Io, host: &impl HostApi, ctx: &UrlCtx<'_>, url: &str) -> Result<Admitted, ApiRefusal> {
    let sw = io.run(Stmt::new(SELECT_SWITCHES, Vec::new())).await;
    let sw = SwitchRows::from_done(sw.as_ref().ok());
    if !sw.on(io, switches::TOOL) {
        return Err(refusal(RefusalCode::ToolOff));
    }
    if let Some(id) = xpost::x_post_v1(url) {
        let x_on = sw.on(io, switches::URLS) && sw.state(io, switches::X) == DetectXState::On.as_str();
        if !x_on {
            return Err(refusal(RefusalCode::XOff));
        }
        return admit_x(io, ctx, id).await;
    }
    if !sw.on(io, switches::URLS) {
        return Err(refusal(RefusalCode::UrlsOff));
    }
    let target = match slop_ninja_detect_url::admit(url, ctx.policy) {
        Ok(UrlAdmitted::Slug(s)) => {
            return match pslug::fold(&s, PUBLIC_LEN) {
                pslug::Folded::Canonical => Ok(Admitted::Done(s)),
                pslug::Folded::Fold(f) => Ok(Admitted::Done(f)),
                pslug::Folded::Invalid => Err(refusal(RefusalCode::NotFound)),
            };
        }
        Ok(UrlAdmitted::Fetch(t)) => t,
        // A link on an X host that the strict post rule did not read is not
        // a post.
        Err(Refusal::PasteOnly) if xish(url) => return Err(refusal(RefusalCode::XNotAPost)),
        Err(r) => return Err(refusal(url_refusal(r))),
    };
    let optout = io.run(Stmt::new(SELECT_OPTOUT, vec![Val::text(&target.host)])).await.map_err(|_| unavailable())?;
    if optout.first().is_some() {
        return Err(refusal(RefusalCode::PrivateSource));
    }
    match host.screen(&target.registrable, &target.host).await {
        Ok(Screen::Clear) => {}
        Ok(Screen::SecurityRisk) => return Err(refusal(RefusalCode::SecurityRisk)),
        Err(IoError) => return Err(unavailable()),
    }
    let submitted_key = platform::source_key(&target.url, &target.platform);
    let alias = alias_key(&submitted_key);
    let ckey = claims::claim_key(&alias, &ctx.serving.load);
    let known = io
        .batch(vec![
            Known::stmt(&submitted_key, &ctx.serving.load),
            Stmt::new(
                SELECT_HELD_FAILURE,
                vec![Val::blob(&ckey), Val::int(io.now_ms().saturating_sub(REFUSAL_HOLD_MS))],
            ),
        ])
        .await
        .map_err(|_| unavailable())?;
    let k = Known::of(known.first());
    if let Some(slug_) = k.live {
        io.metric("reuse", &["alias"]);
        return Ok(Admitted::Done(slug_));
    }
    // A removal or a withheld excerpt stands for the link: its page answers,
    // with no fetch, so sending the link again never undoes it.
    if let Some(slug_) = k.blocked {
        return Ok(Admitted::Done(slug_));
    }
    if k.opted_out {
        return Err(refusal(RefusalCode::RerunBlocked));
    }
    if let Some(slug_) = known.get(1).and_then(Done::first).and_then(|r| r.text("public_slug")) {
        return Ok(Admitted::Done(slug_.to_owned()));
    }
    let platform = match target.platform {
        platform::Platform::Bluesky { .. } => Platform::Bluesky,
        _ => Platform::Web,
    };
    claim_or_join(io, ctx, &ckey, platform, Link::Page { target, submitted_key }).await
}

fn xish(url: &str) -> bool {
    url::Url::parse(url.trim())
        .or_else(|_| url::Url::parse(&format!("https://{}", url.trim())))
        .ok()
        .and_then(|u| u.host_str().map(platform::is_x_host))
        .unwrap_or(false)
}

fn unavailable() -> ApiRefusal {
    refusal(RefusalCode::Unavailable)
}

/// The tombstone or a live page of post `id`.
pub const SELECT_X_SOURCE: &str = "SELECT s.source_id AS source_id, s.canon_url AS canon_url, s.state AS sstate, \
(SELECT d.public_slug FROM detections d WHERE d.source_id = s.source_id AND d.state = 'live' \
AND d.package_load = ?2 AND d.superseded_by IS NULL ORDER BY d.created_at DESC LIMIT 1) AS live_slug \
FROM sources s WHERE s.canon_key = ?1 AND s.adapter = 'x'";

async fn admit_x(io: &impl Io, ctx: &UrlCtx<'_>, id: u64) -> Result<Admitted, ApiRefusal> {
    if let Some(after) = xpost::x_too_new(id, io.now_ms()) {
        io.metric("xread", &["too_new"]);
        return Err(ApiRefusal::x_post_too_new(after));
    }
    let key = x_flow::x_canon_key(id);
    let d = io
        .run(Stmt::new(SELECT_X_SOURCE, vec![Val::blob(&key), Val::text(&ctx.serving.load)]))
        .await
        .map_err(|_| unavailable())?;
    if let Some(r) = d.first() {
        if r.text("canon_url").is_none() && r.text("sstate") == Some("opted_out") {
            return Err(refusal(RefusalCode::XRemoved));
        }
        if let Some(s) = r.text("live_slug") {
            io.metric("reuse", &["alias"]);
            return Ok(Admitted::Done(s.to_owned()));
        }
    }
    let ckey = claims::claim_key(&key, &ctx.serving.load);
    claim_or_join(io, ctx, &ckey, Platform::X, Link::XPost { id }).await
}

async fn claim_or_join(
    io: &impl Io,
    ctx: &UrlCtx<'_>,
    ckey: &Bytes32,
    platform: Platform,
    link: Link,
) -> Result<Admitted, ApiRefusal> {
    match claims::claim(io, ckey, platform, &ctx.serving.load, ctx.lease_ms).await {
        Ok(Claim::Drive(held)) => Ok(Admitted::Drive(Box::new(Drive { held, link }))),
        Ok(Claim::Joined { slug }) => {
            io.metric("reuse", &["claim_join"]);
            Ok(Admitted::Joined(slug))
        }
        Err(IoError) => Err(unavailable()),
    }
}

// ------------------------------------------------------------------ drive

/// Why a check stopped short of a live page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// The page is still to be ended `failed` with this code ([`fail`]).
    Fail(RefusalCode),
    /// The store's own batch already ended the page `failed` with this code.
    Ended(RefusalCode),
}

impl From<RefusalCode> for Stop {
    fn from(c: RefusalCode) -> Stop {
        Stop::Fail(c)
    }
}

/// Runs a claimed check to its end. It never fails: every outcome is the
/// page's state (`live`, or `failed` with its code).
pub async fn drive(
    io: &impl Io,
    host: &impl HostApi,
    fetch: &impl FetchApi,
    scorer: &impl UrlScorer,
    ctx: &UrlCtx<'_>,
    job: &Drive,
) -> Ended {
    let r = match &job.link {
        Link::XPost { id } => drive_x(io, fetch, scorer, ctx, &job.held, *id).await,
        Link::Page { target, submitted_key } => {
            drive_page(io, host, fetch, scorer, ctx, &job.held, target, submitted_key).await
        }
    };
    let slug = job.held.slug.clone();
    let domain = match &job.link {
        Link::Page { target, .. } => Some((target.host.as_str(), adapter_of(target))),
        Link::XPost { .. } => None,
    };
    match r {
        Ok(()) => Ended { slug, failed: None },
        Err(Stop::Ended(code)) => Ended { slug, failed: Some(code) },
        Err(Stop::Fail(code)) => {
            let _ = fail(io, &job.held, code, domain).await;
            Ended { slug, failed: Some(code) }
        }
    }
}

/// The key of a failed check's domain-only source: `host:<host>`. A failed
/// page shows the reason and its domain only, so its source row holds the
/// origin and nothing of the page (no path, title or text).
pub fn domain_key(host: &str) -> String {
    format!("host:{host}")
}

/// The domain-only source of a failed web or Bluesky check.
pub const UPSERT_DOMAIN_SOURCE: &str = "INSERT INTO sources (canon_key, canon_url, host, adapter, state, reputation, \
rank_list_v) VALUES (?1, ?2, ?3, ?4, 'ok', 'unranked', 'none') ON CONFLICT (canon_key) DO NOTHING";

/// Ends the page `failed` with `code` and frees the link, if this driver
/// still holds it. A web or Bluesky page keeps a domain-only source
/// (`domain`: its host and adapter); an X page keeps none.
pub async fn fail(io: &impl Io, held: &Held, code: RefusalCode, domain: Option<(&str, &str)>) -> Result<(), IoError> {
    let g = held_vals(held);
    let mut batch = Vec::new();
    let source = match domain {
        Some((host, adapter)) => {
            let k = source_canon_key(&domain_key(host));
            batch.push(Stmt::new(
                UPSERT_DOMAIN_SOURCE,
                vec![Val::blob(&k), Val::text(format!("https://{host}/")), Val::text(host), Val::text(adapter)],
            ));
            Val::blob(&k)
        }
        None => Val::Null,
    };
    batch.push(Stmt::owned(
        format!(
            "UPDATE detections SET state = 'failed', refusal = ?3, \
source_id = (SELECT source_id FROM sources WHERE canon_key = ?6), run_id = NULL, \
state_v = state_v + 1, updated_at = ?4 WHERE id_h = ?5 AND state = 'pending' AND {FENCE}"
        ),
        vec![g.0.clone(), g.1.clone(), Val::text(code.as_str()), Val::int(io.now_ms()), Val::blob(&held.id_h), source],
    ));
    if matches!(code, RefusalCode::Robots | RefusalCode::PrivateSource) {
        // The failure answers for its link for the robots cache period.
        batch.push(Stmt::owned(
            format!(
                "INSERT INTO refusal_marks (claim_key, det_id_h) SELECT ?1, ?3 WHERE {FENCE} \
ON CONFLICT (claim_key) DO UPDATE SET det_id_h = excluded.det_id_h"
            ),
            vec![g.0.clone(), g.1.clone(), Val::blob(&held.id_h)],
        ));
    }
    batch.push(Stmt::new(claims::RELEASE, vec![g.0, g.1]));
    io.batch(batch).await.map(|_| ()).map_err(IoError::from)
}

fn held_vals(h: &Held) -> (Val, Val) {
    (Val::blob(&h.claim_key), Val::text(&h.token))
}

#[allow(clippy::too_many_arguments)]
async fn drive_page(
    io: &impl Io,
    host: &impl HostApi,
    fetch: &impl FetchApi,
    scorer: &impl UrlScorer,
    ctx: &UrlCtx<'_>,
    held: &Held,
    target: &slop_ninja_detect_url::Target,
    submitted_key: &str,
) -> Result<(), Stop> {
    let url = target.url.as_str();
    let mut slot = host.slot(&target.registrable, url, ctx.lease_ms).await.map_err(|_| RefusalCode::Unavailable)?;
    if let Slot::WaitUntil(t) = slot {
        if t.saturating_sub(io.now_ms()) > ctx.fetch_ms {
            return Err(RefusalCode::TooSlow.into());
        }
        host.wait_until(t).await;
        slot = host.slot(&target.registrable, url, ctx.lease_ms).await.map_err(|_| RefusalCode::Unavailable)?;
    }
    let slot_id = match slot {
        Slot::Granted { slot } => slot,
        Slot::Disallowed => return Err(RefusalCode::Robots.into()),
        Slot::NoSearch => return Err(RefusalCode::PrivateSource.into()),
        Slot::WaitUntil(_) => return Err(RefusalCode::TooSlow.into()),
    };
    let req = FetchRequest {
        url: url.to_owned(),
        slot: slot_id.clone(),
        limits: Limits { html_bound: ctx.html_bound, wait_ms: ctx.fetch_ms, hops: MAX_HOPS },
        conditional: None,
    };
    // A failed call (not a failed fetch) is tried once more: the fetch
    // Worker recycles its isolate after a large page.
    let answer = match fetch.fetch(&req).await {
        Ok(a) => Ok(a),
        Err(IoError) => fetch.fetch(&req).await,
    };
    host.release(&target.registrable, &slot_id, answer.as_ref().ok().and_then(|a| a.retry_after_ms)).await;
    let answer = answer.map_err(|_| RefusalCode::FetchFailed)?;
    io.metric("fetch", &[answer.outcome.code()]);
    let known_key = source_canon_key(submitted_key);
    if answer.outcome != Outcome::Ok {
        let state = match answer.outcome {
            Outcome::PrivateSource => Some("unlisted"),
            Outcome::NotFound => Some("gone"),
            _ => None,
        };
        if let Some(st) = state {
            let _ = io
                .run(Stmt::new(
                    "UPDATE sources SET state = ?2 WHERE canon_key = ?1 AND adapter <> 'x' AND state <> 'opted_out'",
                    vec![Val::blob(&known_key), Val::text(st)],
                ))
                .await;
        }
        let code = answer.outcome.refusal().and_then(RefusalCode::parse).unwrap_or(RefusalCode::FetchFailed);
        return Err(code.into());
    }
    // The site the text came from. A redirect to another host is judged by
    // that host: its opt-out ends the check as the submitted host's would.
    let site = FetchedSite::of(ctx.policy, target, &answer).ok_or(RefusalCode::FetchFailed)?;
    if site.host != target.host {
        let optout = io
            .run(Stmt::new(SELECT_OPTOUT, vec![Val::text(&site.host)]))
            .await
            .map_err(|_| RefusalCode::Unavailable)?;
        if optout.first().is_some() {
            return Err(RefusalCode::PrivateSource.into());
        }
    }
    let text = answer.text.as_deref().ok_or(RefusalCode::FetchFailed)?;
    let canonical = crate::canon::canon_v1(text).into_owned();
    // The source: the final URL, or the submitted one; every form of the
    // link is an alias. The declared canonical is only a dedupe candidate.
    let page = SourcePage::of(target, submitted_key, &answer, &site.registrable);
    // Canonical dedupe: another live page of this source, or of the
    // declared canonical's, under this package.
    let mut q = vec![Known::stmt(&page.key, &ctx.serving.load)];
    if let Some(d) = &page.declared {
        q.push(Known::stmt(d, &ctx.serving.load));
    }
    let dup = io.batch(q).await.map_err(|_| RefusalCode::Unavailable)?;
    let own = Known::of(dup.first());
    let declared = Known::of(dup.get(1));
    let domain = (target.host.as_str(), adapter_of(target));
    // The source this fetch names is under a legal state, or opted out: no
    // new page of it, an alias page included, even while another page of it
    // is live (the store's batch checks again).
    if own.blocked.is_some() || own.opted_out {
        return Err(RefusalCode::RerunBlocked.into());
    }
    if let Some(existing) = own.live.or(declared.live)
        && existing != held.slug
    {
        return alias_to(io, ctx, held, &page, &existing, domain).await;
    }
    let (reputation, rank_v) = match target.platform {
        platform::Platform::Bluesky { .. } => (Reputation::Ranked, "bsky".to_owned()),
        _ => host.reputation(&site.registrable).await.map_err(|_| RefusalCode::Unavailable)?,
    };
    let kind = if matches!(target.platform, platform::Platform::Bluesky { .. }) {
        SourceKind::SocialPost
    } else {
        SourceKind::Web
    };
    let ck = ctx.keys.content_key(CANON_V, &ctx.serving.load, &canonical);
    let reuse = io
        .run(Stmt::new("SELECT run_id FROM runs WHERE content_key = ?1", vec![Val::blob(&ck)]))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    let mut new_run = None;
    let run_id = match reuse.first().and_then(|r| r.u64("run_id")) {
        Some(r) => {
            io.metric("reuse", &["content"]);
            r
        }
        None => {
            claims::renew(io, held, ctx.lease_ms).await.map_err(|_| RefusalCode::Unavailable)?;
            let scored = scorer.score(&canonical).await.map_err(|r| r.code)?;
            let page_words = canonical.split_whitespace().count() as u32;
            let hash = hex::encode(crate::canon::canonical_sha256(&canonical));
            let doc = excerpt::run_doc(&scored.scored, kind, page_words, Some(hash), &scored.result);
            let id = draw_run_id(io);
            io.doc_put(&run_doc_key(id), doc.encode()).await.map_err(|_| RefusalCode::Unavailable)?;
            new_run = Some((id, doc, ck));
            id
        }
    };
    let src = SourceWrite {
        key: page.key.clone(),
        url: page.url.clone(),
        host: site.host.clone(),
        adapter: if kind == SourceKind::SocialPost { "bluesky" } else { "web" },
        meta: answer.meta.clone().unwrap_or_default(),
        reputation,
        rank_v,
        fresh_ms: answer.fresh_ms.unwrap_or(DEFAULT_FRESH_MS),
        etag: answer.etag.clone(),
        last_modified: answer.last_modified.clone(),
        aliases: page.aliases.clone(),
    };
    let web = kind == SourceKind::Web;
    let fence = Fence::Page { submitted: submitted_key, host: domain.0, adapter: domain.1, sitemap: web };
    let stored = store(
        io,
        ctx,
        held,
        &src,
        run_id,
        new_run.as_ref().map(|(id, d, ck)| (*id, d, Some(*ck), answer.extract_v.as_str())),
        fence,
    )
    .await;
    if stored != Ok(Stored::Live)
        && let Some((id, _, _)) = new_run
    {
        // Not ours to show (blocked, taken over, or the store failed): the
        // document must not outlive rows that point at it.
        let _ = io.doc_delete(&[run_doc_key(id)]).await;
    }
    match stored {
        Ok(Stored::Live) => {
            if web {
                let _ = io
                    .enqueue(vec![QueueMessage::new(Job::Indexnow {
                        public_slug: held.slug.clone(),
                        change: slop_ninja_detect_page::codes::IndexChange::Updated,
                    })])
                    .await;
            }
            Ok(())
        }
        Ok(Stored::Blocked) => Err(Stop::Ended(RefusalCode::RerunBlocked)),
        // A taker finishes the page.
        Ok(Stored::Lost) => Ok(()),
        // A store failure: the page ends `failed` if the claim is still
        // ours, else the taker finishes it.
        Err(IoError) => Err(RefusalCode::Unavailable.into()),
    }
}

/// The adapter name of a link's domain-only source.
fn adapter_of(target: &slop_ninja_detect_url::Target) -> &'static str {
    if matches!(target.platform, platform::Platform::Bluesky { .. }) { "bluesky" } else { "web" }
}

/// The site a fetched text came from: the final URL's host and registrable
/// domain, else the submitted link's. A redirect may lead to another site,
/// and every host-level rule reads this one: the host's opt-out, the
/// reputation and family screen, the host the page and its card name
/// (`sources.host`, which the index gates read too), and the site a
/// declared canonical must be on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedSite {
    /// The host, A-label form, as [`slop_ninja_detect_url::Target::host`].
    pub host: String,
    /// Its registrable domain (the `DetectHost` key).
    pub registrable: String,
}

impl FetchedSite {
    /// The final URL read with the same URL rules as a submitted link; a
    /// Bluesky post is its own link's. `None` when the final URL does not
    /// pass them as a web page (the fetch Worker checks every hop with these
    /// rules, so only a mismatch of the two Workers' rules gets here).
    pub fn of(policy: &Policy, target: &slop_ninja_detect_url::Target, a: &FetchAnswer) -> Option<FetchedSite> {
        let own = || FetchedSite { host: target.host.clone(), registrable: target.registrable.clone() };
        if matches!(target.platform, platform::Platform::Bluesky { .. }) {
            return Some(own());
        }
        let Some(f) = a.final_url.as_deref() else { return Some(own()) };
        match slop_ninja_detect_url::admit(f, policy) {
            Ok(UrlAdmitted::Fetch(t)) if t.platform == platform::Platform::Web => {
                Some(FetchedSite { host: t.host, registrable: t.registrable })
            }
            _ => None,
        }
    }
}

/// The source a fetch found, the URL forms that name it, and the declared
/// canonical it may join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePage {
    /// The source key: the final URL's, else the submitted link's.
    pub key: String,
    /// The source's `canon_url`, the URL the text was fetched from.
    pub url: String,
    /// The link's other forms (the submitted and final URLs).
    pub aliases: Vec<String>,
    /// The declared `rel=canonical` on the text's registrable domain
    /// ([`FetchedSite::registrable`]), as a source key, when it differs from
    /// `key`. It is only looked up: this page joins it when it already has a
    /// live page under the serving package, and it never keys, names or
    /// aliases a source, since the page that declares it need not be its
    /// author's.
    pub declared: Option<String>,
}

impl SourcePage {
    /// `registrable`: the registrable domain of the site the text came from.
    pub fn of(
        target: &slop_ninja_detect_url::Target,
        submitted_key: &str,
        a: &FetchAnswer,
        registrable: &str,
    ) -> SourcePage {
        if let (platform::Platform::Bluesky { rkey, .. }, Some(did)) =
            (&target.platform, a.meta.as_ref().and_then(|m| m.did.as_deref()))
        {
            let key = platform::bluesky_key(did, rkey);
            let mut aliases = vec![submitted_key.to_owned()];
            aliases.retain(|k| *k != key);
            return SourcePage { key, url: target.url.as_str().to_owned(), aliases, declared: None };
        }
        let same_site = |u: &str| {
            url::Url::parse(u)
                .ok()
                .and_then(|u| u.host_str().and_then(slop_ninja_detect_url::psl::registrable_domain).map(str::to_owned))
                .is_some_and(|r| r == registrable)
        };
        let chosen = a.final_url.as_deref().unwrap_or(target.url.as_str()).to_owned();
        let key = format!("url:{chosen}");
        let declared =
            a.canonical.as_deref().filter(|c| same_site(c)).map(|d| format!("url:{d}")).filter(|d| *d != key);
        let mut aliases: Vec<String> =
            [Some(submitted_key.to_owned()), a.final_url.as_ref().map(|f| format!("url:{f}"))]
                .into_iter()
                .flatten()
                .filter(|k| *k != key)
                .collect();
        aliases.sort();
        aliases.dedup();
        SourcePage { key, url: chosen, aliases, declared }
    }
}

/// A run id: 52 random bits, positive (the document key is drawn before the
/// row, so R2 comes first).
fn draw_run_id(io: &impl Io) -> u64 {
    let mut b = [0u8; 8];
    io.fill(&mut b);
    (u64::from_le_bytes(b) & ((1u64 << 52) - 1)).max(1)
}

/// The source row a stored check writes.
#[derive(Clone, Debug)]
struct SourceWrite {
    key: String,
    url: String,
    host: String,
    adapter: &'static str,
    meta: slop_ninja_detect_fetch_wire::Meta,
    reputation: Reputation,
    rank_v: String,
    fresh_ms: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    aliases: Vec<String>,
}

/// The source upsert: a check that succeeded leaves the source `ok`, except
/// that an `opted_out` source stays so (no check lifts an opt-out).
pub const UPSERT_SOURCE: &str = "INSERT INTO sources (canon_key, canon_url, host, adapter, title, site_name, \
author, lang, fetched_at, fresh_until, fetch_outcome, etag, last_modified, state, reputation, rank_list_v) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'ok', ?11, ?12, 'ok', ?13, ?14) \
ON CONFLICT (canon_key) DO UPDATE SET canon_url = excluded.canon_url, host = excluded.host, \
title = excluded.title, site_name = excluded.site_name, author = excluded.author, lang = excluded.lang, \
fetched_at = excluded.fetched_at, fresh_until = excluded.fresh_until, fetch_outcome = 'ok', \
etag = excluded.etag, last_modified = excluded.last_modified, \
state = CASE WHEN sources.state = 'opted_out' THEN 'opted_out' ELSE 'ok' END, reputation = excluded.reputation, \
rank_list_v = excluded.rank_list_v WHERE sources.adapter = excluded.adapter";

/// The bounds of the page metadata a source keeps (hostile input: risk F16).
pub const META_CHARS: usize = 300;

fn bounded(s: Option<&String>) -> Val {
    Val::opt_text(s.map(|v| v.chars().filter(|c| !c.is_control()).take(META_CHARS).collect::<String>()))
}

/// Stores a finished check: one D1 batch fenced by the claim's token.
async fn store(
    io: &impl Io,
    ctx: &UrlCtx<'_>,
    held: &Held,
    src: &SourceWrite,
    run_id: u64,
    new_run: Option<(u64, &RunDoc, Option<Bytes32>, &str)>,
    fence: Fence<'_>,
) -> Result<Stored, IoError> {
    let now = io.now_ms();
    let (ck, tok) = held_vals(held);
    let skey = source_canon_key(&src.key);
    let mut b = vec![Stmt::new(
        UPSERT_SOURCE,
        vec![
            Val::blob(&skey),
            Val::text(&src.url),
            Val::text(&src.host),
            Val::text(src.adapter),
            bounded(src.meta.title.as_ref()),
            bounded(src.meta.site_name.as_ref()),
            bounded(src.meta.author.as_ref()),
            bounded(src.meta.lang.as_ref()),
            Val::int(now),
            Val::int(now.saturating_add(src.fresh_ms)),
            bounded(src.etag.as_ref()),
            bounded(src.last_modified.as_ref()),
            Val::text(src.reputation.as_str()),
            Val::text(&src.rank_v),
        ],
    )];
    for a in &src.aliases {
        b.push(Stmt::new(
            "INSERT INTO source_aliases (alias_key, source_id) SELECT ?1, source_id FROM sources WHERE canon_key = ?2 \
ON CONFLICT (alias_key) DO NOTHING",
            vec![Val::blob(&alias_key(a)), Val::blob(&skey)],
        ));
    }
    if let Some((id, doc, content_key, extract_v)) = new_run {
        b.push(insert_run(ctx, &ck, &tok, id, doc, content_key, extract_v));
    }
    finish(io, ctx, held, b, &skey, run_id, new_run.is_some(), now, fence).await
}

/// What may stop a stored check going live. It is checked inside the
/// store's own batch, so a removal that lands while the check runs (the
/// pending page has no source yet, so the removal cannot name it) still
/// wins.
#[derive(Clone, Copy, Debug)]
enum Fence<'a> {
    /// A web page or a Bluesky post: neither the stored source nor the
    /// source the submitted link names may be `opted_out` or have a page
    /// under a legal state ([`BLOCKING_PAGE`]). A blocked page ends
    /// `failed` (`rerun_blocked`) with its domain-only source (`host`,
    /// `adapter`). `sitemap`: the page takes a sitemap position (web only).
    Page { submitted: &'a str, host: &'a str, adapter: &'a str, sitemap: bool },
    /// A post on X: the source must be the live post, not the removal
    /// tombstone. A blocked page ends `failed` (`x_removed`) with no source.
    X,
}

/// How the store's batch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stored {
    /// The page went live under this driver.
    Live,
    /// The fence stopped it: the batch ended the page `failed` and deleted
    /// the run it inserted.
    Blocked,
    /// The claim was taken over: the taker finishes the page.
    Lost,
}

/// Whether a web or Bluesky page is blocked: the stored source (`?4`), or
/// the source the submitted link names by its key (`?8`) or alias (`?9`),
/// is `opted_out` or has a blocking page.
fn page_blocked() -> String {
    format!(
        "EXISTS (SELECT 1 FROM sources s WHERE (s.canon_key IN (?4, ?8) \
OR s.source_id = (SELECT a.source_id FROM source_aliases a WHERE a.alias_key = ?9)) \
AND (s.state = 'opted_out' OR EXISTS (SELECT 1 FROM detections b WHERE {BLOCKING_PAGE})))"
    )
}

/// `x_purge` has run: `DETECT_X` is `purged`.
pub const X_PURGED: &str = "EXISTS (SELECT 1 FROM switches WHERE name = 'DETECT_X' AND state = 'purged')";

/// Whether an X page is blocked: its source (`?4`) is not the live post
/// (the removal left the tombstone, which has no `canon_url`), or X posts
/// were purged as a class while the post was read (so a check that started
/// before `x_purge` never goes live after its pages passed).
pub const X_BLOCKED: &str = "NOT EXISTS (SELECT 1 FROM sources s WHERE s.canon_key = ?4 AND s.canon_url IS NOT NULL \
AND s.state <> 'opted_out') OR EXISTS (SELECT 1 FROM switches WHERE name = 'DETECT_X' AND state = 'purged')";

/// Runs the store's batch: `b` (the source, its aliases, the run) and then
/// [`go_live`]'s statements.
#[allow(clippy::too_many_arguments)]
async fn finish(
    io: &impl Io,
    ctx: &UrlCtx<'_>,
    held: &Held,
    mut b: Vec<Stmt>,
    skey: &Bytes32,
    run_id: u64,
    new_run: bool,
    now: u64,
    fence: Fence<'_>,
) -> Result<Stored, IoError> {
    let (ck, tok) = held_vals(held);
    let at = b.len();
    let (stmts, live, blocked) = go_live(ctx, &ck, &tok, &held.id_h, skey, run_id, new_run, now, fence);
    b.extend(stmts);
    let done = io.batch(b).await?;
    // This driver's own updates moved the page (a taker's would not count).
    let moved = |i: usize| done.get(at + i).is_some_and(|d| d.changes == 1);
    Ok(if moved(live) {
        Stored::Live
    } else if moved(blocked) {
        Stored::Blocked
    } else {
        Stored::Lost
    })
}

#[allow(clippy::too_many_arguments)]
fn insert_run(
    ctx: &UrlCtx<'_>,
    ck: &Val,
    tok: &Val,
    id: u64,
    doc: &RunDoc,
    content_key: Option<Bytes32>,
    extract_v: &str,
) -> Stmt {
    let r = &doc.result_full;
    let band = match (r.band, r.short) {
        (Some(b), _) => b.as_str().to_owned(),
        (None, Some(s)) => format!("short_{}", s.as_str()),
        (None, None) => "none".to_owned(),
    };
    Stmt::owned(
        format!(
            "INSERT INTO runs (run_id, content_key, package_load, package_pub, package_label, policy_v, canon_v, \
extract_v, display_v, evidence_id, band, words, total_words, cut, tokens, scored_at, doc_key, doc_bytes) \
SELECT ?3, ?4, ?5, ?6, ?7, ?8, '{CANON_V}', ?9, '{}', ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18 WHERE {FENCE}",
            crate::display::DISPLAY_V
        ),
        vec![
            ck.clone(),
            tok.clone(),
            Val::int(id),
            Val::opt_blob(content_key.as_ref().map(|k| &k[..])),
            Val::text(&r.package_load),
            Val::text(&r.package_pub),
            Val::text(&r.package_label),
            Val::text(&r.policy),
            Val::text(extract_v),
            Val::text(&ctx.serving.evidence),
            Val::text(band),
            Val::int(u64::from(r.words)),
            Val::int(u64::from(r.total_words)),
            Val::text(r.cut.as_str()),
            Val::int(u64::from(r.tokens)),
            Val::int(r.scored_at),
            Val::text(run_doc_key(id)),
            Val::int(doc.encode().len() as u64),
        ],
    )
}

/// The page goes live unless the fence stops it: its run and source, its
/// sitemap position (web pages only), older live pages of the source
/// superseded and their shards marked dirty, the claim deleted; then the
/// page's state, read back. A blocked page ends `failed` in the same batch
/// and the run this batch inserted (`new_run`) is deleted. Returns the
/// statements and the offsets of the live and the blocked update.
#[allow(clippy::too_many_arguments)]
fn go_live(
    ctx: &UrlCtx<'_>,
    ck: &Val,
    tok: &Val,
    id_h: &Bytes32,
    skey: &Bytes32,
    run_id: u64,
    new_run: bool,
    now: u64,
    fence: Fence<'_>,
) -> (Vec<Stmt>, usize, usize) {
    let sitemap = matches!(fence, Fence::Page { sitemap: true, .. });
    let seq = if sitemap { "(SELECT coalesce(max(sitemap_seq), -1) + 1 FROM detections)" } else { "NULL" };
    let mut live_vals = vec![
        ck.clone(),
        tok.clone(),
        Val::int(run_id),
        Val::blob(skey),
        Val::text(&ctx.serving.load),
        Val::int(now),
        Val::blob(id_h),
    ];
    let blocked = match fence {
        Fence::Page { submitted, .. } => {
            live_vals.push(Val::blob(&source_canon_key(submitted)));
            live_vals.push(Val::blob(&alias_key(submitted)));
            page_blocked()
        }
        Fence::X => X_BLOCKED.to_owned(),
    };
    let mut v = vec![Stmt::owned(
        format!(
            "UPDATE detections SET state = 'live', refusal = NULL, run_id = ?3, \
source_id = (SELECT source_id FROM sources WHERE canon_key = ?4), package_load = ?5, sitemap_seq = {seq}, \
state_v = state_v + 1, updated_at = ?6 WHERE id_h = ?7 AND state = 'pending' AND {FENCE} AND NOT ({blocked})"
        ),
        live_vals,
    )];
    // Still pending with the claim ours after the live update: the fence
    // stopped it. The blocked update is the last of these statements.
    let domain = match fence {
        Fence::Page { host, adapter, .. } => Some((host, adapter)),
        Fence::X => None,
    };
    let end = end_blocked(ck, tok, id_h, now, domain);
    let blocked_at = v.len() + end.len() - 1;
    v.extend(end);
    if new_run {
        v.push(Stmt::new(
            "DELETE FROM runs WHERE run_id = ?1 AND NOT EXISTS (SELECT 1 FROM detections WHERE run_id = ?1)",
            vec![Val::int(run_id)],
        ));
    }
    v.extend([
        Stmt::new(
            "UPDATE detections SET superseded_by = ?1, state_v = state_v + 1 \
WHERE source_id = (SELECT source_id FROM detections WHERE id_h = ?1 AND state = 'live') AND id_h <> ?1 \
AND state = 'live' AND superseded_by IS NULL",
            vec![Val::blob(id_h)],
        ),
        Stmt::new(
            "INSERT INTO sitemap_shards (shard, dirty, urls, built_at) SELECT DISTINCT sitemap_seq / ?2, 1, 0, NULL \
FROM detections WHERE (id_h = ?1 OR superseded_by = ?1) AND sitemap_seq IS NOT NULL \
ON CONFLICT (shard) DO UPDATE SET dirty = 1",
            vec![Val::blob(id_h), Val::int(SHARD_URLS)],
        ),
        Stmt::new(
            "UPDATE sources SET latest_det = ?1 WHERE canon_key = ?2 AND EXISTS \
(SELECT 1 FROM detections WHERE id_h = ?1 AND state = 'live')",
            vec![Val::blob(id_h), Val::blob(skey)],
        ),
        Stmt::new(claims::RELEASE, vec![ck.clone(), tok.clone()]),
        Stmt::new("SELECT state FROM detections WHERE id_h = ?1", vec![Val::blob(id_h)]),
    ]);
    (v, 0, blocked_at)
}

/// Ends a page the fence stopped: `failed`, with no run, while the claim
/// is still this driver's and the page still pending (so after a live
/// update in the same batch it changes nothing). A web or Bluesky page
/// (`domain`: its host and adapter) keeps its domain-only source, inserted
/// only for such a page, and reads `rerun_blocked`; an X page (`None`)
/// keeps no source and reads `x_removed`. The update is the last
/// statement.
fn end_blocked(ck: &Val, tok: &Val, id_h: &Bytes32, now: u64, domain: Option<(&str, &str)>) -> Vec<Stmt> {
    let mut v = Vec::new();
    let (code, source) = match domain {
        Some((host, adapter)) => {
            let k = source_canon_key(&domain_key(host));
            v.push(Stmt::owned(
                format!(
                    "INSERT INTO sources (canon_key, canon_url, host, adapter, state, reputation, rank_list_v) \
SELECT ?3, ?4, ?5, ?6, 'ok', 'unranked', 'none' WHERE {FENCE} \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?7 AND state = 'pending') ON CONFLICT (canon_key) DO NOTHING"
                ),
                vec![
                    ck.clone(),
                    tok.clone(),
                    Val::blob(&k),
                    Val::text(format!("https://{host}/")),
                    Val::text(host),
                    Val::text(adapter),
                    Val::blob(id_h),
                ],
            ));
            (RefusalCode::RerunBlocked, Val::blob(&k))
        }
        None => (RefusalCode::XRemoved, Val::Null),
    };
    v.push(Stmt::owned(
        format!(
            "UPDATE detections SET state = 'failed', refusal = ?3, \
source_id = (SELECT source_id FROM sources WHERE canon_key = ?6), run_id = NULL, \
state_v = state_v + 1, updated_at = ?4 WHERE id_h = ?5 AND state = 'pending' AND {FENCE}"
        ),
        vec![ck.clone(), tok.clone(), Val::text(code.as_str()), Val::int(now), Val::blob(id_h), source],
    ));
    v
}

/// This page becomes an alias of `existing`, a live page of its source or
/// of its declared canonical: the same run and source, `rel=canonical` to
/// it, and the link's forms as aliases of that source. If `existing` is no
/// longer live by then (a removal landed), the page ends `failed`
/// (`rerun_blocked`) with its domain-only source (`domain`: host and
/// adapter).
async fn alias_to(
    io: &impl Io,
    ctx: &UrlCtx<'_>,
    held: &Held,
    page: &SourcePage,
    existing: &str,
    domain: (&str, &str),
) -> Result<(), Stop> {
    let (ck, tok) = held_vals(held);
    let ex = crate::keys::public_id_h(existing);
    let now = io.now_ms();
    let mut b = vec![Stmt::owned(
        format!(
            "UPDATE detections SET state = 'live', refusal = NULL, superseded_by = ?3, \
run_id = (SELECT run_id FROM detections WHERE id_h = ?3), source_id = (SELECT source_id FROM detections WHERE id_h = ?3), \
package_load = ?4, state_v = state_v + 1, updated_at = ?5 WHERE id_h = ?6 AND state = 'pending' AND {FENCE} \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?3 AND state = 'live')"
        ),
        vec![
            ck.clone(),
            tok.clone(),
            Val::blob(&ex),
            Val::text(&ctx.serving.load),
            Val::int(now),
            Val::blob(&held.id_h),
        ],
    )];
    // The link's forms, its own source key among them, name the joined
    // source, only once this page shows it.
    for a in std::iter::once(&page.key).chain(&page.aliases) {
        b.push(Stmt::new(
            "INSERT INTO source_aliases (alias_key, source_id) SELECT ?1, source_id FROM detections \
WHERE id_h = ?2 AND state = 'live' AND source_id IS NOT NULL ON CONFLICT (alias_key) DO NOTHING",
            vec![Val::blob(&alias_key(a)), Val::blob(&held.id_h)],
        ));
    }
    let end = end_blocked(&ck, &tok, &held.id_h, now, Some(domain));
    let blocked_at = b.len() + end.len() - 1;
    b.extend(end);
    b.push(Stmt::new(claims::RELEASE, vec![ck, tok]));
    let done = io.batch(b).await.map_err(|_| RefusalCode::Unavailable)?;
    if done.first().is_some_and(|d| d.changes == 1) {
        io.metric("reuse", &["alias"]);
        Ok(())
    } else if done.get(blocked_at).is_some_and(|d| d.changes == 1) {
        Err(Stop::Ended(RefusalCode::RerunBlocked))
    } else {
        Ok(())
    }
}

// ------------------------------------------------------------------- X

async fn drive_x(
    io: &impl Io,
    fetch: &impl FetchApi,
    scorer: &impl UrlScorer,
    ctx: &UrlCtx<'_>,
    held: &Held,
    id: u64,
) -> Result<(), Stop> {
    let key = x_flow::x_canon_key(id);
    let ans = fetch.x_post(&XPostRequest { id, deadline_ms: ctx.fetch_ms }).await.unwrap_or(XPostAnswer::Busy);
    let text = match ans {
        XPostAnswer::Text { text } => text,
        XPostAnswer::Long => return Err(x_refused(io, "long", RefusalCode::XPostLong).into()),
        XPostAnswer::Busy => return Err(x_refused(io, "busy", RefusalCode::XBusy).into()),
        XPostAnswer::Unexpected => return Err(x_refused(io, "unexpected", RefusalCode::XUnavailable).into()),
        XPostAnswer::Unavailable => {
            // An existing source the embed no longer shows: due at once, so
            // the next sweep checks it with its controls and deletes it.
            let _ = io
                .run(Stmt::new(
                    "UPDATE sources SET fetched_at = 0 WHERE canon_key = ?1 AND adapter = 'x' AND canon_url IS NOT NULL",
                    vec![Val::blob(&key)],
                ))
                .await;
            return Err(x_refused(io, "unavailable", RefusalCode::XPostUnavailable).into());
        }
    };
    io.metric("xread", &["text"]);
    let canonical = crate::canon::canon_v1(&text).into_owned();
    drop(text);
    claims::renew(io, held, ctx.lease_ms).await.map_err(|_| RefusalCode::Unavailable)?;
    let scored = scorer.score(&canonical).await.map_err(|r| r.code)?;
    drop(canonical);
    let doc = excerpt::x_run_doc(&scored.result);
    let run_id = draw_run_id(io);
    io.doc_put(&run_doc_key(run_id), doc.encode()).await.map_err(|_| RefusalCode::Unavailable)?;
    let now = io.now_ms();
    let (ck, tok) = held_vals(held);
    let url = x_canonical(id).ok_or(RefusalCode::Internal)?;
    // The post's source: a tombstone left by a removal is kept as it is,
    // and the fence then ends the page. After `x_purge` no X source is
    // written at all.
    let mut b = vec![Stmt::owned(
        format!(
            "INSERT INTO sources (canon_key, canon_url, host, adapter, fetched_at, fetch_outcome, state, reputation, \
rank_list_v) SELECT ?1, ?2, ?3, 'x', ?4, 'ok', 'ok', 'ranked', 'x' WHERE NOT {X_PURGED} \
ON CONFLICT (canon_key) DO UPDATE SET \
canon_url = excluded.canon_url, fetched_at = excluded.fetched_at, fetch_outcome = 'ok', check_queued_at = NULL, \
state = 'ok', reputation = 'ranked' WHERE sources.adapter = 'x' AND sources.canon_url IS NOT NULL AND NOT {X_PURGED}"
        ),
        vec![Val::blob(&key), Val::text(url), Val::text(X_HOST), Val::int(now)],
    )];
    b.push(insert_run(ctx, &ck, &tok, run_id, &doc, None, X_TEXT_V));
    let stored = finish(io, ctx, held, b, &key, run_id, true, now, Fence::X).await;
    if stored != Ok(Stored::Live) {
        // The document must not outlive rows that point at it.
        let _ = io.doc_delete(&[run_doc_key(run_id)]).await;
    }
    match stored {
        Ok(Stored::Live) => Ok(()),
        // A removal left the tombstone while the post was read, or X posts
        // were purged (whose pages answer `x_purged` anyway).
        Ok(Stored::Blocked) => Err(Stop::Ended(RefusalCode::XRemoved)),
        Ok(Stored::Lost) | Err(IoError) => Err(RefusalCode::Unavailable.into()),
    }
}

fn x_refused(io: &impl Io, outcome: &'static str, code: RefusalCode) -> RefusalCode {
    io.metric("xread", &[outcome]);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_refusals_map_to_page_codes() {
        assert_eq!(url_refusal(Refusal::OwnHost), RefusalCode::BadUrl);
        assert_eq!(url_refusal(Refusal::PasteOnly), RefusalCode::PasteOnly);
        assert_eq!(url_refusal(Refusal::XNotAPost), RefusalCode::XNotAPost);
        assert!(xish("x.com/someone") && xish("https://t.co/a") && !xish("https://example.com/a"));
    }
}

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
//!    package answers its slug (no fetch, no scan), and a recent `robots` or
//!    `private_source` failure answers its failed page for the robots cache
//!    period;
//! 5. the claim ([`crate::claims`]): the winner draws the slug;
//! 6. (`drive`) a politeness slot from `DetectHost` (robots.txt first when
//!    stale; `robots` when it disallows), then `DETECT_FETCH /fetch` (once
//!    more when the call itself fails), every hop re-checked there;
//! 7. a failed fetch ends the page `failed` with the fetch's code, keeping no
//!    text; a source that asks not to be indexed marks a known source
//!    `unlisted`, and a 404 or 410 marks it `gone`;
//! 8. reputation (ranking shards and the family screen);
//! 9. canonical dedupe: the final or declared canonical URL that is already
//!    a live source under this package makes this page an alias of that one
//!    (same run, `rel=canonical` to it) and answers its slug;
//! 10. `canon_v1`, then `content_key` reuse (public text only), else the
//!     scorer (the floor, the English screen, the free cut, a gate ticket of
//!     class `url`);
//! 11. storage: the run document `r/<run_id>` (passages, anchors, offsets;
//!     never the full text) first, then one D1 batch fenced by the claim's
//!     token: the source, its aliases, the run, the page `live` with its
//!     `sitemap_seq`, older pages of the source superseded, the shards marked
//!     dirty, the claim deleted; an IndexNow ping is queued.
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
//!    `reputation = 'ranked'`, `fetched_at` now. No sitemap, no IndexNow.

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

/// The source of a key, by its own key or an alias, and its newest page
/// under `package` (`live` first, then a recent hold-worthy failure).
pub const SELECT_KNOWN: &str = "SELECT s.source_id AS source_id, s.state AS sstate, s.canon_url AS canon_url, \
(SELECT d.public_slug FROM detections d WHERE d.source_id = s.source_id AND d.kind = 'public_url' \
AND d.state = 'live' AND d.package_load = ?3 AND d.superseded_by IS NULL ORDER BY d.created_at DESC LIMIT 1) \
AS live_slug FROM sources s WHERE s.canon_key = ?1 \
OR s.source_id = (SELECT a.source_id FROM source_aliases a WHERE a.alias_key = ?2) LIMIT 1";

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
    let canon = source_canon_key(&submitted_key);
    let alias = alias_key(&submitted_key);
    let ckey = claims::claim_key(&alias, &ctx.serving.load);
    let known = io
        .batch(vec![
            Stmt::new(SELECT_KNOWN, vec![Val::blob(&canon), Val::blob(&alias), Val::text(&ctx.serving.load)]),
            Stmt::new(
                SELECT_HELD_FAILURE,
                vec![Val::blob(&ckey), Val::int(io.now_ms().saturating_sub(REFUSAL_HOLD_MS))],
            ),
        ])
        .await
        .map_err(|_| unavailable())?;
    if let Some(slug_) = known.first().and_then(Done::first).and_then(|r| r.text("live_slug")) {
        io.metric("reuse", &["alias"]);
        return Ok(Admitted::Done(slug_.to_owned()));
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
        Link::Page { target, .. } => Some((
            target.host.as_str(),
            if matches!(target.platform, platform::Platform::Bluesky { .. }) { "bluesky" } else { "web" },
        )),
        Link::XPost { .. } => None,
    };
    match r {
        Ok(()) => Ended { slug, failed: None },
        Err(code) => {
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
) -> Result<(), RefusalCode> {
    let url = target.url.as_str();
    let mut slot = host.slot(&target.registrable, url, ctx.lease_ms).await.map_err(|_| RefusalCode::Unavailable)?;
    if let Slot::WaitUntil(t) = slot {
        if t.saturating_sub(io.now_ms()) > ctx.fetch_ms {
            return Err(RefusalCode::TooSlow);
        }
        host.wait_until(t).await;
        slot = host.slot(&target.registrable, url, ctx.lease_ms).await.map_err(|_| RefusalCode::Unavailable)?;
    }
    let slot_id = match slot {
        Slot::Granted { slot } => slot,
        Slot::Disallowed => return Err(RefusalCode::Robots),
        Slot::NoSearch => return Err(RefusalCode::PrivateSource),
        Slot::WaitUntil(_) => return Err(RefusalCode::TooSlow),
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
                    "UPDATE sources SET state = ?2 WHERE canon_key = ?1 AND adapter <> 'x'",
                    vec![Val::blob(&known_key), Val::text(st)],
                ))
                .await;
        }
        let code = answer.outcome.refusal().and_then(RefusalCode::parse).unwrap_or(RefusalCode::FetchFailed);
        return Err(code);
    }
    let text = answer.text.as_deref().ok_or(RefusalCode::FetchFailed)?;
    let canonical = crate::canon::canon_v1(text).into_owned();
    // The source: the declared canonical on the same registrable domain, or
    // the final URL, or the submitted one; every form is an alias.
    let page = SourcePage::of(target, submitted_key, &answer);
    // Canonical dedupe: another live page of this source under this package.
    let dup = io
        .run(Stmt::new(
            SELECT_KNOWN,
            vec![
                Val::blob(&source_canon_key(&page.key)),
                Val::blob(&alias_key(&page.key)),
                Val::text(&ctx.serving.load),
            ],
        ))
        .await
        .map_err(|_| RefusalCode::Unavailable)?;
    if let Some(existing) = dup.first().and_then(|r| r.text("live_slug")).map(str::to_owned)
        && existing != held.slug
    {
        return alias_to(io, ctx, held, &page, &existing).await;
    }
    let (reputation, rank_v) = match target.platform {
        platform::Platform::Bluesky { .. } => (Reputation::Ranked, "bsky".to_owned()),
        _ => host.reputation(&target.registrable).await.map_err(|_| RefusalCode::Unavailable)?,
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
        host: target.host.clone(),
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
    let live = store(
        io,
        ctx,
        held,
        &src,
        run_id,
        new_run.as_ref().map(|(id, d, ck)| (*id, d, Some(*ck), answer.extract_v.as_str())),
        web,
    )
    .await;
    match live {
        Ok(true) => {
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
        Ok(false) | Err(_) => {
            if let Some((id, _, _)) = new_run {
                // Not ours to store (taken over, or the store failed): the
                // document must not outlive rows that point at it.
                let _ = io.doc_delete(&[run_doc_key(id)]).await;
            }
            // A taker finishes the page; a store failure leaves it pending
            // with its lease, so "Run it" can retry.
            if live.is_err() { Err(RefusalCode::Unavailable) } else { Ok(()) }
        }
    }
}

/// The source a fetch found, and the URL forms that name it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePage {
    pub key: String,
    pub url: String,
    pub aliases: Vec<String>,
}

impl SourcePage {
    pub fn of(target: &slop_ninja_detect_url::Target, submitted_key: &str, a: &FetchAnswer) -> SourcePage {
        if let (platform::Platform::Bluesky { rkey, .. }, Some(did)) =
            (&target.platform, a.meta.as_ref().and_then(|m| m.did.as_deref()))
        {
            let key = platform::bluesky_key(did, rkey);
            let mut aliases = vec![submitted_key.to_owned()];
            aliases.retain(|k| *k != key);
            return SourcePage { key, url: target.url.as_str().to_owned(), aliases };
        }
        let same_site = |u: &str| {
            url::Url::parse(u)
                .ok()
                .and_then(|u| u.host_str().and_then(slop_ninja_detect_url::psl::registrable_domain).map(str::to_owned))
                .is_some_and(|r| r == target.registrable)
        };
        let declared = a.canonical.as_deref().filter(|c| same_site(c));
        let chosen = declared.or(a.final_url.as_deref()).unwrap_or(target.url.as_str()).to_owned();
        let key = format!("url:{chosen}");
        let mut aliases: Vec<String> = [
            Some(submitted_key.to_owned()),
            a.final_url.as_ref().map(|f| format!("url:{f}")),
            declared.map(|d| format!("url:{d}")),
        ]
        .into_iter()
        .flatten()
        .filter(|k| *k != key)
        .collect();
        aliases.sort();
        aliases.dedup();
        SourcePage { key, url: chosen, aliases }
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

/// The source upsert: a check that succeeded leaves the source `ok`.
const UPSERT_SOURCE: &str = "INSERT INTO sources (canon_key, canon_url, host, adapter, title, site_name, author, \
lang, fetched_at, fresh_until, fetch_outcome, etag, last_modified, state, reputation, rank_list_v) \
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'ok', ?11, ?12, 'ok', ?13, ?14) \
ON CONFLICT (canon_key) DO UPDATE SET canon_url = excluded.canon_url, host = excluded.host, \
title = excluded.title, site_name = excluded.site_name, author = excluded.author, lang = excluded.lang, \
fetched_at = excluded.fetched_at, fresh_until = excluded.fresh_until, fetch_outcome = 'ok', \
etag = excluded.etag, last_modified = excluded.last_modified, state = 'ok', reputation = excluded.reputation, \
rank_list_v = excluded.rank_list_v WHERE sources.adapter = excluded.adapter";

/// The bounds of the page metadata a source keeps (hostile input: risk F16).
pub const META_CHARS: usize = 300;

fn bounded(s: Option<&String>) -> Val {
    Val::opt_text(s.map(|v| v.chars().filter(|c| !c.is_control()).take(META_CHARS).collect::<String>()))
}

/// Stores a finished check: one D1 batch fenced by the claim's token.
/// `Ok(true)` when the page went live under this driver.
async fn store(
    io: &impl Io,
    ctx: &UrlCtx<'_>,
    held: &Held,
    src: &SourceWrite,
    run_id: u64,
    new_run: Option<(u64, &RunDoc, Option<Bytes32>, &str)>,
    sitemap: bool,
) -> Result<bool, IoError> {
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
    let ours = b.len();
    b.extend(go_live(ctx, &ck, &tok, &held.id_h, &skey, run_id, now, sitemap));
    let done = io.batch(b).await?;
    // This driver's own update moved the page (a taker's would not count).
    Ok(done.get(ours).is_some_and(|d| d.changes == 1))
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

/// The page goes live: its run and source, its sitemap position (web pages
/// only), older pages of the source superseded and their shards marked
/// dirty, the claim deleted; then the page's state, read back.
#[allow(clippy::too_many_arguments)]
fn go_live(
    ctx: &UrlCtx<'_>,
    ck: &Val,
    tok: &Val,
    id_h: &Bytes32,
    skey: &Bytes32,
    run_id: u64,
    now: u64,
    sitemap: bool,
) -> Vec<Stmt> {
    let seq = if sitemap { "(SELECT coalesce(max(sitemap_seq), -1) + 1 FROM detections)" } else { "NULL" };
    vec![
        Stmt::owned(
            format!(
                "UPDATE detections SET state = 'live', refusal = NULL, run_id = ?3, \
source_id = (SELECT source_id FROM sources WHERE canon_key = ?4), package_load = ?5, sitemap_seq = {seq}, \
state_v = state_v + 1, updated_at = ?6 WHERE id_h = ?7 AND state = 'pending' AND {FENCE}"
            ),
            vec![
                ck.clone(),
                tok.clone(),
                Val::int(run_id),
                Val::blob(skey),
                Val::text(&ctx.serving.load),
                Val::int(now),
                Val::blob(id_h),
            ],
        ),
        Stmt::new(
            "UPDATE detections SET superseded_by = ?1, state_v = state_v + 1 \
WHERE source_id = (SELECT source_id FROM detections WHERE id_h = ?1 AND state = 'live') AND id_h <> ?1 \
AND state IN ('live','excerpt_withheld') AND superseded_by IS NULL",
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
    ]
}

/// This page becomes an alias of `existing`, the live page its source
/// already has: the same run, `rel=canonical` to it, and the link's forms as
/// aliases of its source.
async fn alias_to(
    io: &impl Io,
    ctx: &UrlCtx<'_>,
    held: &Held,
    page: &SourcePage,
    existing: &str,
) -> Result<(), RefusalCode> {
    let (ck, tok) = held_vals(held);
    let skey = source_canon_key(&page.key);
    let ex = crate::keys::public_id_h(existing);
    let now = io.now_ms();
    let mut b = Vec::new();
    for a in &page.aliases {
        b.push(Stmt::new(
            "INSERT INTO source_aliases (alias_key, source_id) SELECT ?1, source_id FROM sources WHERE canon_key = ?2 \
ON CONFLICT (alias_key) DO NOTHING",
            vec![Val::blob(&alias_key(a)), Val::blob(&skey)],
        ));
    }
    b.push(Stmt::owned(
        format!(
            "UPDATE detections SET state = 'live', refusal = NULL, superseded_by = ?3, \
run_id = (SELECT run_id FROM detections WHERE id_h = ?3), source_id = (SELECT source_id FROM detections WHERE id_h = ?3), \
package_load = ?4, state_v = state_v + 1, updated_at = ?5 WHERE id_h = ?6 AND state = 'pending' AND {FENCE} \
AND EXISTS (SELECT 1 FROM detections WHERE id_h = ?3 AND state = 'live')"
        ),
        vec![ck.clone(), tok.clone(), Val::blob(&ex), Val::text(&ctx.serving.load), Val::int(now), Val::blob(&held.id_h)],
    ));
    b.push(Stmt::new(claims::RELEASE, vec![ck, tok]));
    io.batch(b).await.map_err(|_| RefusalCode::Unavailable)?;
    io.metric("reuse", &["alias"]);
    Ok(())
}

// ------------------------------------------------------------------- X

async fn drive_x(
    io: &impl Io,
    fetch: &impl FetchApi,
    scorer: &impl UrlScorer,
    ctx: &UrlCtx<'_>,
    held: &Held,
    id: u64,
) -> Result<(), RefusalCode> {
    let key = x_flow::x_canon_key(id);
    let ans = fetch.x_post(&XPostRequest { id, deadline_ms: ctx.fetch_ms }).await.unwrap_or(XPostAnswer::Busy);
    let text = match ans {
        XPostAnswer::Text { text } => text,
        XPostAnswer::Long => return Err(x_refused(io, "long", RefusalCode::XPostLong)),
        XPostAnswer::Busy => return Err(x_refused(io, "busy", RefusalCode::XBusy)),
        XPostAnswer::Unexpected => return Err(x_refused(io, "unexpected", RefusalCode::XUnavailable)),
        XPostAnswer::Unavailable => {
            // An existing source the embed no longer shows: due at once, so
            // the next sweep checks it with its controls and deletes it.
            let _ = io
                .run(Stmt::new(
                    "UPDATE sources SET fetched_at = 0 WHERE canon_key = ?1 AND adapter = 'x' AND canon_url IS NOT NULL",
                    vec![Val::blob(&key)],
                ))
                .await;
            return Err(x_refused(io, "unavailable", RefusalCode::XPostUnavailable));
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
    let mut b = vec![Stmt::new(
        "INSERT INTO sources (canon_key, canon_url, host, adapter, fetched_at, fetch_outcome, state, reputation, \
rank_list_v) VALUES (?1, ?2, ?3, 'x', ?4, 'ok', 'ok', 'ranked', 'x') ON CONFLICT (canon_key) DO UPDATE SET \
canon_url = excluded.canon_url, fetched_at = excluded.fetched_at, fetch_outcome = 'ok', check_queued_at = NULL, \
state = 'ok', reputation = 'ranked' WHERE sources.adapter = 'x' AND sources.canon_url IS NOT NULL",
        vec![Val::blob(&key), Val::text(url), Val::text(X_HOST), Val::int(now)],
    )];
    b.push(insert_run(ctx, &ck, &tok, run_id, &doc, None, X_TEXT_V));
    let ours = b.len();
    b.extend(go_live(ctx, &ck, &tok, &held.id_h, &key, run_id, now, false));
    let ok = match io.batch(b).await {
        Ok(done) => done.get(ours).is_some_and(|d| d.changes == 1),
        Err(_) => false,
    };
    if !ok {
        let _ = io.doc_delete(&[run_doc_key(run_id)]).await;
        return Err(RefusalCode::Unavailable);
    }
    Ok(())
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

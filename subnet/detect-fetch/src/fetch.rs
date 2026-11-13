//! One page fetch and extraction (DESIGN 7.5 step 6).
//!
//! 1. The request's URL passes the URL rules again (the detect Worker
//!    admitted it; a Bluesky post goes to [`crate::bluesky`]).
//! 2. `GET` with no cookies, no `Authorization` and no referrer, the fixed
//!    `Accept`, the SlopNinjaBot user agent and Web Bot Auth headers (risk
//!    F5), and the validators of a re-check. One wall-clock budget covers
//!    every hop, robots read, body and extraction (risk F12); no hidden
//!    retries.
//! 3. Redirects ([`crate::redirects`]): every hop re-checked by the URL
//!    rules, its host screened when new ([`crate::screen`]), its origin's
//!    robots.txt read and its path checked ([`crate::robots`]; the first
//!    URL's robots were checked by `DetectHost`, and any later hop, even on
//!    the same origin, is checked here).
//! 4. Status: 2xx reads the body; 304 answers `not_modified` to a re-check;
//!    401, 407 and a Cloudflare challenge are a login wall; 404 and 410
//!    `not_found`; anything else `fetch_failed`, with any `Retry-After`.
//! 5. `X-Robots-Tag` (general and addressed to SlopNinjaBot) and the TDM
//!    reservation header are read before the body: `noindex`, `none`, `noai`
//!    or a reservation end the fetch `private_source` unread.
//! 6. HTML and XHTML stream through the strip pass ([`crate::strip`]) into
//!    at most the stripped bound; the robots meta tags and the TDM meta tag
//!    are read there; the shape check refuses deep or element-dense pages;
//!    then [`crate::extract`]. A paywall marker, or a password field on a
//!    page with little text, is a login wall. Plain text is read whole within
//!    the same bound.

use crate::bound::{DEPTH_BOUND, EXTRACT_V, HTML_BOUND, LOGIN_WALL_WORDS, MAX_ELEMENTS};
use crate::extract::{self, ExtractError};
use crate::fresh::{fresh_ms, retry_after_ms};
use crate::net::{Head, Headers, Net, NetError, body_within};
use crate::redirects::{self, Kind, Next, is_redirect, origin, path_and_query};
use crate::screen::{Screen, screen};
use crate::sign::Signer;
use crate::strip::{self, Directives, StripError, Stripper};
use slop_ninja_detect_fetch_wire::robots::Decision;
use slop_ninja_detect_fetch_wire::{
    ACCEPT, Adapter, FETCH_WIRE_V, FetchAnswer, FetchRequest, MAX_HOPS, Meta, Outcome, PRODUCT_TOKEN, USER_AGENT,
};
use slop_ninja_detect_url::{Admitted, Platform, Policy, Refusal, admit};
use url::Url;

/// The longest wall-clock budget a request may ask for: the route's own
/// abandonment deadline bounds every invocation anyway.
pub const MAX_WAIT_MS: u64 = 120_000;

/// What every request of one invocation shares.
pub struct Ctx<'a> {
    pub policy: &'a Policy,
    /// `None` only in dev builds (the Worker refuses to fetch unsigned in a
    /// release build).
    pub signer: Option<&'a Signer>,
}

/// The headers of one outbound request to `url`.
pub fn request_headers<N: Net>(net: &mut N, ctx: &Ctx<'_>, url: &Url, accept: &str) -> Headers {
    let mut h: Headers = vec![("user-agent", USER_AGENT.to_owned()), ("accept", accept.to_owned())];
    if let Some(s) = ctx.signer {
        let mut nonce = [0u8; 32];
        net.random(&mut nonce);
        let created = net.now_ms() / 1000;
        h.extend(s.headers(url.host_str().unwrap_or(""), created, &nonce));
    }
    h
}

/// An answer with only its outcome.
pub fn bare(outcome: Outcome, adapter: Adapter) -> FetchAnswer {
    FetchAnswer {
        v: FETCH_WIRE_V.to_owned(),
        outcome,
        adapter,
        final_url: None,
        canonical: None,
        meta: None,
        text: None,
        etag: None,
        last_modified: None,
        fresh_ms: None,
        retry_after_ms: None,
        hops: 0,
        body_bytes: 0,
        stripped_bytes: 0,
        extract_v: EXTRACT_V.to_owned(),
    }
}

fn web(outcome: Outcome, hops: u32) -> FetchAnswer {
    FetchAnswer { hops, ..bare(outcome, Adapter::Web) }
}

/// A body kind by `Content-Type`.
enum BodyKind {
    Html(Option<String>),
    Plain(Option<String>),
}

fn content_kind(v: Option<&str>) -> Option<BodyKind> {
    let v = v?;
    let mut parts = v.split(';');
    let mime = parts.next()?.trim().to_ascii_lowercase();
    let charset = parts.find_map(|p| {
        let (k, val) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| val.trim().trim_matches('"').to_owned())
    });
    match mime.as_str() {
        "text/html" | "application/xhtml+xml" => Some(BodyKind::Html(charset)),
        "text/plain" => Some(BodyKind::Plain(charset)),
        _ => None,
    }
}

/// Whether a refusal is a bot challenge (Cloudflare marks its own).
fn challenge(head: &Head) -> bool {
    head.get("cf-mitigated").is_some_and(|v| v.trim().eq_ignore_ascii_case("challenge"))
}

/// Runs one fetch. `on_stripped` is told the stripped page's bytes and
/// elements before extraction (the Worker gives back the reservation the
/// page does not need).
pub async fn fetch<N: Net>(net: &mut N, ctx: &Ctx<'_>, req: &FetchRequest, on_stripped: &mut dyn FnMut(usize, usize)) -> FetchAnswer {
    let start = net.now_ms();
    let deadline = start.saturating_add(req.limits.wait_ms.min(MAX_WAIT_MS));
    let max_hops = req.limits.hops.min(MAX_HOPS);
    let html_bound = usize::try_from(req.limits.html_bound).unwrap_or(usize::MAX).min(HTML_BOUND);
    let target = match admit(&req.url, ctx.policy) {
        Ok(Admitted::Fetch(t)) => t,
        Err(Refusal::PasteOnly) => return bare(Outcome::Unsupported, Adapter::Web),
        Ok(Admitted::Slug(_)) | Err(_) => return bare(Outcome::BadUrl, Adapter::Web),
    };
    if let Platform::Bluesky { actor, rkey } = &target.platform {
        return crate::bluesky::fetch(net, ctx, &target.url, actor, rkey, deadline, html_bound).await;
    }
    let mut url = target.url;
    let mut screened = vec![target.host];
    let mut robots: Vec<(String, slop_ninja_detect_fetch_wire::robots::RobotsRules)> = Vec::new();
    let mut seen = vec![url.to_string()];
    let mut hops = 0u32;
    let conditional = req.conditional.as_ref();
    let head = loop {
        let mut headers = request_headers(net, ctx, &url, ACCEPT);
        if hops == 0
            && let Some(c) = conditional
        {
            if let Some(e) = &c.etag {
                headers.push(("if-none-match", e.clone()));
            }
            if let Some(l) = &c.last_modified {
                headers.push(("if-modified-since", l.clone()));
            }
        }
        let head = match net.get(url.as_str(), &headers, deadline).await {
            Ok(h) => h,
            Err(NetError::Timeout) => return web(Outcome::TooSlow, hops),
            Err(NetError::Transport) => return web(Outcome::FetchFailed, hops),
        };
        if !is_redirect(head.status) {
            break head;
        }
        net.discard();
        let (next, host) = match redirects::next(&url, head.get("location"), ctx.policy, Kind::Page, hops, max_hops, &seen) {
            Next::Go { url, host } => (url, host),
            Next::Stop(o) => return web(o, hops),
        };
        hops += 1;
        if !screened.contains(&host) && !net.screen_exempt(&host) {
            match screen(net, &host, deadline).await {
                Ok(Screen::Clear) => screened.push(host),
                Ok(Screen::Blocked) => return web(Outcome::SecurityRisk, hops),
                Ok(Screen::Unknown) | Err(NetError::Transport) => return web(Outcome::FetchFailed, hops),
                Err(NetError::Timeout) => return web(Outcome::TooSlow, hops),
            }
        }
        let o = origin(&next);
        if !robots.iter().any(|(k, _)| *k == o) {
            let Ok(o_url) = Url::parse(&o) else { return web(Outcome::RedirectRefused, hops) };
            let a = crate::robots::read(net, ctx, &o_url, &mut screened, deadline).await;
            if net.now_ms() > deadline {
                return web(Outcome::TooSlow, hops);
            }
            robots.push((o.clone(), a.rules));
        }
        let decision = robots.iter().find(|(k, _)| *k == o).map_or(Decision::Disallowed, |(_, r)| r.decide(&path_and_query(&next)));
        match decision {
            Decision::Allowed => {}
            Decision::Disallowed | Decision::NoAiInput => return web(Outcome::Robots, hops),
            Decision::NoSearch => return web(Outcome::PrivateSource, hops),
        }
        seen.push(next.to_string());
        url = next;
    };
    let now = net.now_ms();
    let base = FetchAnswer {
        final_url: Some(url.to_string()),
        etag: head.get("etag").map(|v| v.trim().to_owned()),
        last_modified: head.get("last-modified").map(|v| v.trim().to_owned()),
        fresh_ms: fresh_ms(&head, now),
        retry_after_ms: retry_after_ms(&head, now),
        ..web(Outcome::Ok, hops)
    };
    let fail = |o: Outcome| FetchAnswer { outcome: o, ..base.clone() };
    match head.status {
        200..=299 if head.status != 204 && head.status != 206 => {}
        304 if hops == 0 && conditional.is_some() => {
            net.discard();
            return fail(Outcome::NotModified);
        }
        401 | 407 => {
            net.discard();
            return fail(Outcome::LoginWall);
        }
        403 | 503 if challenge(&head) => {
            net.discard();
            return fail(Outcome::LoginWall);
        }
        404 | 410 => {
            net.discard();
            return fail(Outcome::NotFound);
        }
        _ => {
            net.discard();
            return fail(Outcome::FetchFailed);
        }
    }
    let mut directives = Directives::default();
    for v in head.all("x-robots-tag") {
        directives.apply_header(v, PRODUCT_TOKEN);
    }
    if directives.private() || head.get("tdm-reservation").is_some_and(|v| v.trim() == "1") {
        net.discard();
        return fail(Outcome::PrivateSource);
    }
    let Some(kind) = content_kind(head.get("content-type")) else {
        net.discard();
        return fail(Outcome::UnsupportedType);
    };
    match kind {
        BodyKind::Plain(charset) => {
            let body = match body_within(net, html_bound, deadline).await {
                Ok(Some(b)) => b,
                Ok(None) => return fail(Outcome::PageTooLarge),
                Err(NetError::Timeout) => return fail(Outcome::TooSlow),
                Err(NetError::Transport) => return fail(Outcome::FetchFailed),
            };
            on_stripped(body.len(), 0);
            let text = extract::paragraphs(&strip::decode_plain(&body, charset.as_deref()));
            if text.is_empty() {
                return fail(Outcome::ExtractFailed);
            }
            FetchAnswer {
                text: Some(text),
                body_bytes: body.len() as u64,
                stripped_bytes: body.len() as u64,
                meta: Some(Meta { nosnippet: directives.nosnippet, noarchive: directives.noarchive, ..Meta::default() }),
                ..base
            }
        }
        BodyKind::Html(charset) => {
            let mut st = Stripper::new(html_bound, charset.as_deref());
            let read = net.read_body(deadline, &mut |c| st.write(c)).await;
            match read {
                Ok(()) => {}
                Err(NetError::Timeout) => return fail(Outcome::TooSlow),
                Err(NetError::Transport) => return fail(Outcome::FetchFailed),
            }
            let stripped = match st.finish() {
                Ok(s) => s,
                Err(StripError::TooLarge | StripError::TooManyElements) => return fail(Outcome::PageTooLarge),
                Err(StripError::TooDeep) => return fail(Outcome::TooDeep),
                Err(StripError::Parse) => return fail(Outcome::FetchFailed),
            };
            let base = FetchAnswer { body_bytes: stripped.body_bytes, stripped_bytes: stripped.html.len() as u64, ..base };
            let fail = |o: Outcome| FetchAnswer { outcome: o, ..base.clone() };
            directives.merge(stripped.meta.robots);
            if directives.private() || stripped.meta.tdm_reservation {
                return fail(Outcome::PrivateSource);
            }
            if stripped.meta.paywalled {
                return fail(Outcome::LoginWall);
            }
            let shape = match strip::shape(&stripped.html, DEPTH_BOUND, MAX_ELEMENTS) {
                Ok(s) => s,
                Err(StripError::TooDeep) => return fail(Outcome::TooDeep),
                Err(_) => return fail(Outcome::PageTooLarge),
            };
            if !crate::bound::fits(stripped.html.len(), shape.elements) {
                return fail(Outcome::PageTooLarge);
            }
            on_stripped(stripped.html.len(), shape.elements);
            if net.now_ms() > deadline {
                return fail(Outcome::TooSlow);
            }
            let ex = match extract::extract(&stripped.html, url.as_str()) {
                Ok(e) => e,
                Err(ExtractError::TooManyElements) => return fail(Outcome::PageTooLarge),
                Err(ExtractError::NoContent) if stripped.meta.password_field => return fail(Outcome::LoginWall),
                Err(ExtractError::NoContent) => return fail(Outcome::ExtractFailed),
            };
            if ex.words < LOGIN_WALL_WORDS && stripped.meta.password_field {
                return fail(Outcome::LoginWall);
            }
            let canonical =
                stripped.meta.canonical.as_deref().and_then(|href| url.join(href).ok()).and_then(|u| match admit(u.as_str(), ctx.policy) {
                    Ok(Admitted::Fetch(t)) => Some(t.url.to_string()),
                    _ => None,
                });
            let m = stripped.meta;
            let meta = Meta {
                title: m.og_title.or(m.title).or(ex.title),
                site_name: m.site_name.or(ex.site_name),
                author: m.author.or(ex.byline),
                published: m.published.or(ex.published),
                lang: m.lang.or(ex.lang),
                nosnippet: directives.nosnippet,
                noarchive: directives.noarchive,
                did: None,
            };
            FetchAnswer { text: Some(ex.text), canonical, meta: Some(meta), ..base }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use crate::extract::tests::article;
    use crate::net::fake::{FakeNet, Reply};
    use crate::screen::doh_url;
    use slop_ninja_detect_fetch_wire::{Conditional, Limits};

    const CLEAR: &str = r#"{"Status":0,"Answer":[{"type":1,"data":"192.0.2.1"}]}"#;
    const BLOCKED: &str = r#"{"Status":0,"Answer":[{"type":1,"data":"0.0.0.0"}]}"#;

    fn req(url: &str) -> FetchRequest {
        FetchRequest {
            url: url.into(),
            slot: "s".into(),
            limits: Limits { html_bound: u64::MAX, wait_ms: 30_000, hops: 20 },
            conditional: None,
        }
    }

    fn run_with(net: &mut FakeNet, r: &FetchRequest, signer: Option<&Signer>) -> FetchAnswer {
        let policy = Policy::production();
        let ctx = Ctx { policy: &policy, signer };
        block_on(fetch(net, &ctx, r, &mut |_, _| {}))
    }

    fn run(net: &mut FakeNet, url: &str) -> FetchAnswer {
        run_with(net, &req(url), None)
    }

    #[test]
    fn a_plain_article_is_extracted() {
        let mut net = FakeNet::new().site(
            "https://example.com/a",
            Reply::new(
                200,
                &[("content-type", "text/html; charset=utf-8"), ("etag", "\"v1\""), ("cache-control", "max-age=300")],
                article(8, 50),
            ),
        );
        let a = run(&mut net, "https://example.com/a");
        assert_eq!(a.outcome, Outcome::Ok);
        assert!(a.text.as_deref().unwrap().contains("quiet harbour"));
        assert_eq!(a.final_url.as_deref(), Some("https://example.com/a"));
        assert_eq!(a.etag.as_deref(), Some("\"v1\""));
        assert_eq!(a.fresh_ms, Some(300_000));
        assert_eq!(a.meta.as_ref().unwrap().title.as_deref(), Some("A synthetic article"));
        assert!(a.stripped_bytes > 0 && a.body_bytes >= a.stripped_bytes);
        // The request carried only the fixed headers.
        let (_, h) = &net.requests[0];
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["user-agent", "accept"]);
        assert_eq!(h[1].1, ACCEPT);
    }

    #[test]
    fn requests_are_signed_when_a_key_is_set() {
        let key = crate::sign::tests::test_jwk();
        let signer = Signer::from_jwk(&key, "https://slop.ninja").unwrap();
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&article(4, 50)));
        let a = run_with(&mut net, &req("https://example.com/a"), Some(&signer));
        assert_eq!(a.outcome, Outcome::Ok);
        let names: Vec<&str> = net.requests[0].1.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["user-agent", "accept", "signature-agent", "signature-input", "signature"]);
        for (k, _) in &net.requests[0].1 {
            assert!(!["cookie", "authorization", "referer"].contains(&k.as_str()));
        }
    }

    #[test]
    fn redirects_are_rechecked_screened_and_robots_checked() {
        // A same-origin hop: robots.txt of the origin is read here.
        let mut net = FakeNet::new()
            .site("https://example.com/a", Reply::redirect(301, "/b"))
            .site("https://example.com/robots.txt", Reply::new(200, &[], "User-agent: *\nDisallow: /b\n"))
            .site("https://example.com/b", Reply::html(&article(4, 50)));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::Robots);
        // A new host: screened, then its robots read, then fetched.
        let mut net = FakeNet::new()
            .site("https://example.com/a", Reply::redirect(302, "https://www.example.org/story"))
            .site(&doh_url("www.example.org"), Reply::new(200, &[], CLEAR))
            .site("https://www.example.org/robots.txt", Reply::new(404, &[], ""))
            .site("https://www.example.org/story", Reply::html(&article(4, 50)));
        let a = run(&mut net, "https://example.com/a");
        assert_eq!((a.outcome, a.hops), (Outcome::Ok, 1));
        assert_eq!(a.final_url.as_deref(), Some("https://www.example.org/story"));
        assert_eq!(
            net.urls(),
            [
                "https://example.com/a",
                doh_url("www.example.org").as_str(),
                "https://www.example.org/robots.txt",
                "https://www.example.org/story"
            ]
        );
        // A blocked hop host.
        let mut net = FakeNet::new()
            .site("https://example.com/a", Reply::redirect(302, "https://bad.example.net/"))
            .site(&doh_url("bad.example.net"), Reply::new(200, &[], BLOCKED));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::SecurityRisk);
        // An unanswered screen stops the fetch.
        let mut net = FakeNet::new().site("https://example.com/a", Reply::redirect(302, "https://other.example.net/"));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::FetchFailed);
        // Downgrade and private-address hops.
        let mut net = FakeNet::new().site("https://example.com/a", Reply::redirect(302, "http://example.com/a"));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::Downgrade);
        let mut net = FakeNet::new().site("https://example.com/a", Reply::redirect(307, "https://169.254.169.254/latest/"));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::RedirectRefused);
        let mut net = FakeNet::new().site("https://example.com/a", Reply::redirect(307, "https://example.com/verify/abc"));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::RedirectRefused);
        // A loop and the hop limit.
        let mut net = FakeNet::new()
            .site("https://example.com/a", Reply::redirect(302, "/b"))
            .site("https://example.com/robots.txt", Reply::new(404, &[], ""))
            .site("https://example.com/b", Reply::redirect(302, "/a"));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::TooManyRedirects);
        let mut net = FakeNet::new().site("https://example.com/robots.txt", Reply::new(404, &[], ""));
        for k in 0..30 {
            net = net.site(&format!("https://example.com/p{k}"), Reply::redirect(302, &format!("/p{}", k + 1)));
        }
        let mut r = req("https://example.com/p0");
        r.limits.hops = 3;
        let a = run_with(&mut net, &r, None);
        assert_eq!((a.outcome, a.hops), (Outcome::TooManyRedirects, 3));
        // http to https is not a downgrade.
        let mut net = FakeNet::new()
            .site("http://example.com/a", Reply::redirect(301, "https://example.com/a"))
            .site("https://example.com/robots.txt", Reply::new(404, &[], ""))
            .site("https://example.com/a", Reply::html(&article(4, 50)));
        assert_eq!(run(&mut net, "http://example.com/a").outcome, Outcome::Ok);
    }

    #[test]
    fn noindex_in_the_header_or_meta_keeps_no_text() {
        for headers in [
            &[("content-type", "text/html"), ("x-robots-tag", "noindex")][..],
            &[("content-type", "text/html"), ("x-robots-tag", "SlopNinjaBot: noai")][..],
            &[("content-type", "text/html"), ("tdm-reservation", "1")][..],
        ] {
            let mut net = FakeNet::new().site("https://example.com/a", Reply::new(200, headers, article(4, 50)));
            let a = run(&mut net, "https://example.com/a");
            assert_eq!(a.outcome, Outcome::PrivateSource, "{headers:?}");
            assert!(a.text.is_none());
        }
        let mut net = FakeNet::new().site(
            "https://example.com/a",
            Reply::new(200, &[("content-type", "text/html"), ("x-robots-tag", "googlebot: noindex")], article(4, 50)),
        );
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::Ok);
        let page = article(4, 50).replace("<head>", "<head><meta name=\"robots\" content=\"noindex\">");
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&page));
        let a = run(&mut net, "https://example.com/a");
        assert_eq!(a.outcome, Outcome::PrivateSource);
        assert!(a.text.is_none());
        let page = article(4, 50).replace("<head>", "<head><meta name=\"robots\" content=\"nosnippet\">");
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&page));
        assert!(run(&mut net, "https://example.com/a").meta.unwrap().nosnippet);
    }

    #[test]
    fn statuses_types_and_walls() {
        for (status, headers, want) in [
            (404, &[("content-type", "text/html")][..], Outcome::NotFound),
            (410, &[][..], Outcome::NotFound),
            (401, &[][..], Outcome::LoginWall),
            (403, &[("cf-mitigated", "challenge")][..], Outcome::LoginWall),
            (403, &[][..], Outcome::FetchFailed),
            (500, &[][..], Outcome::FetchFailed),
            (200, &[("content-type", "application/pdf")][..], Outcome::UnsupportedType),
            (200, &[("content-type", "image/png")][..], Outcome::UnsupportedType),
            (200, &[][..], Outcome::UnsupportedType),
        ] {
            let mut net = FakeNet::new().site("https://example.com/a", Reply::new(status, headers, "x"));
            assert_eq!(run(&mut net, "https://example.com/a").outcome, want, "{status} {headers:?}");
        }
        let mut net = FakeNet::new().site("https://example.com/a", Reply::new(429, &[("retry-after", "30")], ""));
        let a = run(&mut net, "https://example.com/a");
        assert_eq!((a.outcome, a.retry_after_ms), (Outcome::FetchFailed, Some(30_000)));
        // A login form with little text, and a paywall marker.
        let login = "<html><body><form><input type=\"text\"><input type=\"password\"></form><p>Sign in to continue.</p></body></html>";
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(login));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::LoginWall);
        let pay = article(6, 50).replace("</head>", "<script type=\"application/ld+json\">{\"isAccessibleForFree\":false}</script></head>");
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&pay));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::LoginWall);
        // Plain text.
        let mut net = FakeNet::new()
            .site("https://example.com/a.txt", Reply::new(200, &[("content-type", "text/plain")], "Line one\ncontinues.\n\nSecond  para."));
        assert_eq!(run(&mut net, "https://example.com/a.txt").text.as_deref(), Some("Line one continues.\n\nSecond para."));
        // A conditional re-check.
        let mut net = FakeNet::new().site("https://example.com/a", Reply::new(304, &[("etag", "\"v1\"")], ""));
        let mut r = req("https://example.com/a");
        r.conditional = Some(Conditional { etag: Some("\"v1\"".into()), last_modified: None });
        assert_eq!(run_with(&mut net, &r, None).outcome, Outcome::NotModified);
        assert!(net.requests[0].1.iter().any(|(k, v)| k == "if-none-match" && v == "\"v1\""));
    }

    #[test]
    fn bounds_and_budget() {
        // The stripped bound refuses; it never truncates.
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&article(200, 100)));
        let mut r = req("https://example.com/a");
        r.limits.html_bound = 16 * 1024;
        let a = run_with(&mut net, &r, None);
        assert_eq!(a.outcome, Outcome::PageTooLarge);
        assert!(a.text.is_none());
        // Deep nesting is refused before extraction.
        let deep = format!("<html><body>{}<p>x</p></body></html>", "<div>".repeat(DEPTH_BOUND + 10));
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&deep));
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::TooDeep);
        // The wall-clock budget covers every request.
        let mut net = FakeNet::new()
            .site("https://example.com/a", Reply::redirect(302, "/b"))
            .site("https://example.com/robots.txt", Reply::new(404, &[], ""))
            .site("https://example.com/b", Reply::html(&article(4, 50)));
        net.tick_ms = 4_000;
        let mut r = req("https://example.com/a");
        r.limits.wait_ms = 10_000;
        assert_eq!(run_with(&mut net, &r, None).outcome, Outcome::TooSlow);
        let mut net = FakeNet::new().site("https://example.com/a", Reply::timeout());
        assert_eq!(run(&mut net, "https://example.com/a").outcome, Outcome::TooSlow);
    }

    #[test]
    fn the_request_url_is_rechecked() {
        let mut net = FakeNet::new();
        assert_eq!(run(&mut net, "https://127.0.0.1/").outcome, Outcome::BadUrl);
        assert_eq!(run(&mut net, "https://slop.ninja/d/abc1234").outcome, Outcome::BadUrl);
        assert_eq!(run(&mut net, "https://x.com/u/status/1").outcome, Outcome::Unsupported);
        assert!(net.requests.is_empty());
    }

    #[test]
    fn the_canonical_link_passes_the_url_rules() {
        let page = article(4, 50).replace("<head>", "<head><link rel=\"canonical\" href=\"/canon?utm_source=x\">");
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&page));
        assert_eq!(run(&mut net, "https://example.com/a").canonical.as_deref(), Some("https://example.com/canon"));
        let page = article(4, 50).replace("<head>", "<head><link rel=\"canonical\" href=\"https://127.0.0.1/x\">");
        let mut net = FakeNet::new().site("https://example.com/a", Reply::html(&page));
        assert_eq!(run(&mut net, "https://example.com/a").canonical, None);
    }
}

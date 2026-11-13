//! One robots.txt read under RFC 9309 section 2.3.1 (risk F8).
//!
//! - `GET <origin>/robots.txt` as SlopNinjaBot (signed), following at most
//!   five redirects, each passing the URL input checks and the security
//!   screen (risks F1-F3); more than five means "unavailable" (2.3.1.2).
//! - 2xx: the first 500 KiB are parsed into the group that applies to
//!   SlopNinjaBot (section 2.5).
//! - 4xx other than 429: "unavailable", everything allowed (2.3.1.3).
//! - 429, 5xx, a network failure or the budget running out: "unreachable",
//!   everything disallowed (2.3.1.4).
//! - A hop refused by the URL rules or the screen: treated as unreachable.
//!
//! The answer may be cached for 24 hours at most (section 2.4), less when
//! the response's own freshness says so; an unreachable answer for
//! [`UNREACHABLE_CACHE_MS`], so the next request tries again soon.

use crate::fetch::{Ctx, request_headers};
use crate::fresh::fresh_ms;
use crate::net::Net;
use crate::redirects::{self, Kind, Next, is_redirect};
use crate::screen::{Screen, screen};
use slop_ninja_detect_fetch_wire::robots::{ROBOTS_CACHE_MS, ROBOTS_PARSE_BYTES, ROBOTS_V, RobotsRules};
use slop_ninja_detect_fetch_wire::{FETCH_WIRE_V, PRODUCT_TOKEN, ROBOTS_HOPS, RobotsAnswer, RobotsOutcome};
use url::Url;

/// How long an unreachable answer may be cached.
pub const UNREACHABLE_CACHE_MS: u64 = 5 * 60 * 1000;

fn answer(outcome: RobotsOutcome, rules: RobotsRules, cache_ms: u64, hops: u32) -> RobotsAnswer {
    RobotsAnswer { v: FETCH_WIRE_V.to_owned(), outcome, rules, cache_ms, hops }
}

fn unreachable(hops: u32) -> RobotsAnswer {
    answer(RobotsOutcome::Unreachable, RobotsRules::disallow_all(), UNREACHABLE_CACHE_MS, hops)
}

/// Reads the robots.txt of `origin` (`scheme://host`, already checked by
/// the caller) before `deadline_ms`; `screened` lists hosts already screened
/// (the origin's own host among them) and gains every new hop's host.
pub async fn read<N: Net>(net: &mut N, ctx: &Ctx<'_>, origin: &Url, screened: &mut Vec<String>, deadline_ms: u64) -> RobotsAnswer {
    let Ok(mut url) = origin.join("/robots.txt") else { return unreachable(0) };
    let mut hops = 0u32;
    let mut seen = vec![url.to_string()];
    loop {
        let headers = request_headers(net, ctx, &url, "text/plain");
        let head = match net.get(url.as_str(), &headers, deadline_ms).await {
            Ok(h) => h,
            Err(_) => return unreachable(hops),
        };
        if is_redirect(head.status) {
            net.discard();
            if hops >= ROBOTS_HOPS {
                return answer(RobotsOutcome::Unavailable, RobotsRules::allow_all(), ROBOTS_CACHE_MS, hops);
            }
            match redirects::next(&url, head.get("location"), ctx.policy, Kind::Robots, hops, ROBOTS_HOPS, &seen) {
                Next::Go { url: next, host } => {
                    hops += 1;
                    if !screened.contains(&host) && !net.screen_exempt(&host) {
                        match screen(net, &host, deadline_ms).await {
                            Ok(Screen::Clear) => screened.push(host),
                            Ok(Screen::Blocked) => {
                                return answer(RobotsOutcome::Refused, RobotsRules::disallow_all(), UNREACHABLE_CACHE_MS, hops);
                            }
                            Ok(Screen::Unknown) | Err(_) => return unreachable(hops),
                        }
                    }
                    seen.push(next.to_string());
                    url = next;
                    continue;
                }
                Next::Stop(slop_ninja_detect_fetch_wire::Outcome::TooManyRedirects) => {
                    return answer(RobotsOutcome::Unavailable, RobotsRules::allow_all(), ROBOTS_CACHE_MS, hops);
                }
                Next::Stop(_) => return answer(RobotsOutcome::Refused, RobotsRules::disallow_all(), UNREACHABLE_CACHE_MS, hops),
            }
        }
        let cache = fresh_ms(&head, net.now_ms()).map_or(ROBOTS_CACHE_MS, |f| f.min(ROBOTS_CACHE_MS));
        return match head.status {
            200..=299 => {
                let mut body = Vec::new();
                let read = net
                    .read_body(deadline_ms, &mut |chunk| {
                        let room = ROBOTS_PARSE_BYTES.saturating_sub(body.len());
                        body.extend_from_slice(chunk.get(..room.min(chunk.len())).unwrap_or(chunk));
                        body.len() < ROBOTS_PARSE_BYTES
                    })
                    .await;
                match read {
                    Ok(()) => {
                        let mut rules = RobotsRules::parse(&body, PRODUCT_TOKEN);
                        rules.v = ROBOTS_V.to_owned();
                        answer(RobotsOutcome::Fetched, rules, cache, hops)
                    }
                    Err(_) => unreachable(hops),
                }
            }
            429 => {
                net.discard();
                unreachable(hops)
            }
            400..=499 => {
                net.discard();
                answer(RobotsOutcome::Unavailable, RobotsRules::allow_all(), cache, hops)
            }
            _ => {
                net.discard();
                unreachable(hops)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on;
    use crate::net::fake::{FakeNet, Reply};
    use crate::screen::doh_url;
    use slop_ninja_detect_url::Policy;

    fn run(net: &mut FakeNet) -> RobotsAnswer {
        let policy = Policy::production();
        let ctx = Ctx { policy: &policy, signer: None };
        let origin = Url::parse("https://example.com").unwrap();
        let mut screened = vec!["example.com".to_owned()];
        block_on(read(net, &ctx, &origin, &mut screened, net.now + 60_000))
    }

    #[test]
    fn status_classes() {
        let mut net = FakeNet::new().site("https://example.com/robots.txt", Reply::new(200, &[], "User-agent: *\nDisallow: /private/\n"));
        let a = run(&mut net);
        assert_eq!(a.outcome, RobotsOutcome::Fetched);
        assert!(!a.rules.allows("/private/x") && a.rules.allows("/public"));
        assert_eq!(a.cache_ms, ROBOTS_CACHE_MS);
        assert_eq!(
            net.requests[0].1.iter().find(|(k, _)| k == "user-agent").map(|(_, v)| v.as_str()),
            Some(slop_ninja_detect_fetch_wire::USER_AGENT)
        );
        for (status, want, allowed) in [
            (404, RobotsOutcome::Unavailable, true),
            (401, RobotsOutcome::Unavailable, true),
            (403, RobotsOutcome::Unavailable, true),
            (429, RobotsOutcome::Unreachable, false),
            (500, RobotsOutcome::Unreachable, false),
            (503, RobotsOutcome::Unreachable, false),
        ] {
            let mut net = FakeNet::new().site("https://example.com/robots.txt", Reply::new(status, &[], "x"));
            let a = run(&mut net);
            assert_eq!(a.outcome, want, "{status}");
            assert_eq!(a.rules.allows("/x"), allowed, "{status}");
        }
        let mut net = FakeNet::new().site("https://example.com/robots.txt", Reply::timeout());
        assert_eq!(run(&mut net).outcome, RobotsOutcome::Unreachable);
    }

    #[test]
    fn redirects_are_checked_and_screened() {
        let ok_dns = Reply::new(200, &[], r#"{"Status":0,"Answer":[{"type":1,"data":"192.0.2.1"}]}"#);
        let mut net = FakeNet::new()
            .site("https://example.com/robots.txt", Reply::redirect(301, "https://www.example.com/robots.txt"))
            .site(&doh_url("www.example.com"), ok_dns)
            .site(
                "https://www.example.com/robots.txt",
                Reply::new(200, &[("cache-control", "max-age=60")], "User-agent: SlopNinjaBot\nDisallow: /\n"),
            );
        let a = run(&mut net);
        assert_eq!((a.outcome, a.hops, a.cache_ms), (RobotsOutcome::Fetched, 1, 60_000));
        assert!(!a.rules.allows("/x"));
        // A hop to a private address is refused: everything disallowed.
        let mut net = FakeNet::new().site("https://example.com/robots.txt", Reply::redirect(302, "https://10.0.0.1/robots.txt"));
        let a = run(&mut net);
        assert_eq!(a.outcome, RobotsOutcome::Refused);
        assert!(!a.rules.allows("/x"));
        // A blocked hop host.
        let mut net = FakeNet::new()
            .site("https://example.com/robots.txt", Reply::redirect(302, "https://bad.example.net/robots.txt"))
            .site(&doh_url("bad.example.net"), Reply::new(200, &[], r#"{"Status":0,"Answer":[{"type":1,"data":"0.0.0.0"}]}"#));
        assert_eq!(run(&mut net).outcome, RobotsOutcome::Refused);
        // More than five redirects: unavailable, everything allowed.
        let mut net = FakeNet::new();
        for k in 0..7 {
            let from = if k == 0 { "https://example.com/robots.txt".to_owned() } else { format!("https://example.com/r{k}") };
            net = net.site(&from, Reply::redirect(302, &format!("/r{}", k + 1)));
        }
        let a = run(&mut net);
        assert_eq!(a.outcome, RobotsOutcome::Unavailable);
        assert!(a.rules.allows("/x"));
    }

    #[test]
    fn only_the_parse_bound_is_read() {
        let mut body = "User-agent: *\nDisallow: /a\n".to_owned();
        while body.len() < ROBOTS_PARSE_BYTES + 1000 {
            body.push_str("# filler filler filler filler filler filler filler\n");
        }
        body.push_str("Disallow: /late\n");
        let mut net = FakeNet::new().site("https://example.com/robots.txt", Reply::new(200, &[], body));
        net.window = 4096;
        let a = run(&mut net);
        assert!(!a.rules.allows("/a") && a.rules.allows("/late"));
    }
}

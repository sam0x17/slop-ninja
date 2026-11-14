//! The rules of the `DetectHost` Durable Object (pure; the object itself is
//! `cf_host`, wasm32 only).
//!
//! One object per registrable domain (its name is the domain). It never sees
//! a page body. It keeps, in its own storage:
//!
//! - **robots.txt**, parsed, per origin: read through `DETECT_FETCH
//!   /robots` when missing or stale, cached for at most 24 hours (RFC 9309
//!   section 2.4) or less when the answer says so; a 4xx means allowed and a
//!   5xx means disallowed (the fetch Worker's answer already says which);
//! - **the politeness slot**: one fetch at a time per domain, a structural
//!   choice, held at most the claim's lease; beyond that only the site's own
//!   `Crawl-delay` or a `Retry-After` it sent;
//! - **the security screen** of each host (1.1.1.1 for Families' security
//!   resolver), cached for the answer's DNS TTL;
//! - **the reputation** of the domain (the ranking shards and the family
//!   screen), cached for a day and keyed by the ranking list's version.
//!
//! Different domains are fetched in parallel; their objects share nothing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slop_ninja_detect_fetch_wire::robots::{Decision, RobotsRules};
use slop_ninja_detect_page::codes::Reputation;

use crate::net::Slot;

/// RFC 9309's longest cache period.
pub const ROBOTS_MAX_CACHE_MS: u64 = 24 * 3_600_000;
/// The shortest and longest the screen answer is cached, whatever its TTL
/// (a TTL of zero would query on every check; a long one would hide a new
/// listing).
pub const SCREEN_MIN_TTL_S: u64 = 60;
pub const SCREEN_MAX_TTL_S: u64 = 86_400;
/// How long a domain's reputation is cached (the ranking list is weekly).
pub const REPUTATION_CACHE_MS: u64 = 24 * 3_600_000;
/// Origins and hosts one object keeps before it evicts the oldest entry: a
/// cache, so an eviction costs only one more read.
pub const CACHE_ENTRIES: usize = 64;

/// The DoH JSON endpoints of the two screens.
pub const SECURITY_DOH: &str = "https://security.cloudflare-dns.com/dns-query";
pub const FAMILY_DOH: &str = "https://family.cloudflare-dns.com/dns-query";

/// A cached robots.txt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RobotsEntry {
    pub rules: RobotsRules,
    pub until: u64,
}

/// A cached screen answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenEntry {
    pub blocked: bool,
    pub until: u64,
}

/// A cached reputation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepEntry {
    pub reputation: Reputation,
    pub list_v: String,
    pub until: u64,
}

/// The slot one fetch holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub id: String,
    pub origin: String,
    pub until: u64,
}

/// An object's whole state (one storage value besides the robots rules,
/// which are stored per origin: a large rule set needs its own value).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostState {
    pub slot: Option<Held>,
    /// No fetch starts before this time (`Crawl-delay`, `Retry-After`).
    pub next_at: u64,
    pub screens: BTreeMap<String, ScreenEntry>,
    pub reputation: Option<RepEntry>,
}

/// What the object answers a slot request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlotStep {
    /// Read the origin's robots.txt first, then ask again.
    NeedRobots,
    Answer(Slot),
}

/// The origin (`scheme://host[:port]`) and path-and-query of `url`.
pub fn origin_and_path(url: &str) -> Option<(String, String)> {
    let u = url::Url::parse(url).ok()?;
    let origin = u.origin().ascii_serialization();
    let mut path = u.path().to_owned();
    if let Some(q) = u.query() {
        path.push('?');
        path.push_str(q);
    }
    Some((origin, path))
}

impl HostState {
    /// A slot for `path` on `origin`, under `robots` (the origin's cached
    /// rules, `None` when missing). `id` names a granted slot.
    pub fn slot(
        &mut self,
        now: u64,
        origin: &str,
        path: &str,
        robots: Option<&RobotsEntry>,
        lease_ms: u64,
        id: String,
    ) -> SlotStep {
        let Some(r) = robots.filter(|r| r.until > now) else { return SlotStep::NeedRobots };
        match r.rules.decide(path) {
            Decision::Allowed => {}
            Decision::Disallowed | Decision::NoAiInput => return SlotStep::Answer(Slot::Disallowed),
            Decision::NoSearch => return SlotStep::Answer(Slot::NoSearch),
        }
        if let Some(h) = self.slot.as_ref().filter(|h| h.until > now) {
            return SlotStep::Answer(Slot::WaitUntil(h.until.max(self.next_at)));
        }
        if self.next_at > now {
            return SlotStep::Answer(Slot::WaitUntil(self.next_at));
        }
        self.slot = Some(Held { id: id.clone(), origin: origin.to_owned(), until: now.saturating_add(lease_ms) });
        SlotStep::Answer(Slot::Granted { slot: id })
    }

    /// The fetch under `id` ended: the slot frees, and the next fetch waits
    /// for the site's `Crawl-delay` (from `robots`) or its `Retry-After`,
    /// whichever is longer. A stale or unknown id changes nothing.
    pub fn release(&mut self, now: u64, id: &str, robots: Option<&RobotsEntry>, retry_after_ms: Option<u64>) {
        if self.slot.as_ref().is_none_or(|h| h.id != id) {
            return;
        }
        self.slot = None;
        let delay = robots.and_then(|r| r.rules.crawl_delay_ms).unwrap_or(0).max(retry_after_ms.unwrap_or(0));
        self.next_at = self.next_at.max(now.saturating_add(delay));
    }

    /// A cached screen answer for `host`, if still fresh.
    pub fn screen(&self, now: u64, host: &str) -> Option<bool> {
        self.screens.get(host).filter(|e| e.until > now).map(|e| e.blocked)
    }

    /// Keeps a screen answer for its TTL (bounded), evicting the entry that
    /// expires first when full.
    pub fn put_screen(&mut self, now: u64, host: &str, blocked: bool, ttl_s: u64) {
        let ttl = ttl_s.clamp(SCREEN_MIN_TTL_S, SCREEN_MAX_TTL_S);
        self.screens.retain(|_, e| e.until > now);
        if self.screens.len() >= CACHE_ENTRIES
            && !self.screens.contains_key(host)
            && let Some(k) = self.screens.iter().min_by_key(|(_, e)| e.until).map(|(k, _)| k.clone())
        {
            self.screens.remove(&k);
        }
        self.screens.insert(host.to_owned(), ScreenEntry { blocked, until: now.saturating_add(ttl * 1000) });
    }

    /// The cached reputation under ranking list `list_v`, if still fresh.
    pub fn reputation(&self, now: u64, list_v: &str) -> Option<Reputation> {
        self.reputation.as_ref().filter(|r| r.until > now && r.list_v == list_v).map(|r| r.reputation)
    }

    pub fn put_reputation(&mut self, now: u64, reputation: Reputation, list_v: &str) {
        self.reputation =
            Some(RepEntry { reputation, list_v: list_v.to_owned(), until: now.saturating_add(REPUTATION_CACHE_MS) });
    }
}

/// A robots.txt answer as the object keeps it.
pub fn robots_entry(now: u64, rules: RobotsRules, cache_ms: u64) -> RobotsEntry {
    RobotsEntry { rules, until: now.saturating_add(cache_ms.min(ROBOTS_MAX_CACHE_MS)) }
}

/// One DoH JSON answer: whether the resolver blocked the name (an `A`
/// answer of `0.0.0.0` or an `AAAA` of `::`), and the shortest TTL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DohAnswer {
    pub blocked: bool,
    pub ttl_s: u64,
}

#[derive(Deserialize)]
struct DohJson {
    #[serde(rename = "Status")]
    status: u32,
    #[serde(rename = "Answer", default)]
    answer: Vec<DohRecord>,
}

#[derive(Deserialize)]
struct DohRecord {
    #[serde(rename = "type")]
    rtype: u32,
    #[serde(rename = "TTL", default)]
    ttl: u64,
    #[serde(default)]
    data: String,
}

/// Parses a DoH JSON answer (`application/dns-json`). `None` when it does
/// not parse or the resolver could not answer (SERVFAIL, REFUSED, ...):
/// a screen that cannot answer stops the check rather than passing it.
/// NXDOMAIN is an answer: the name is not listed (the fetch then fails on
/// its own).
pub fn parse_doh(body: &[u8]) -> Option<DohAnswer> {
    let j: DohJson = serde_json::from_slice(body).ok()?;
    match j.status {
        0 | 3 => {}
        _ => return None,
    }
    let blocked = j.answer.iter().any(|r| (r.rtype == 1 && r.data == "0.0.0.0") || (r.rtype == 28 && r.data == "::"));
    let ttl_s = j.answer.iter().map(|r| r.ttl).min().unwrap_or(SCREEN_MIN_TTL_S);
    Some(DohAnswer { blocked, ttl_s })
}

/// The DoH query URL of `host` on `endpoint`.
pub fn doh_url(endpoint: &str, host: &str) -> String {
    format!("{endpoint}?name={host}&type=A")
}

/// The domain's reputation from its ranking and the family screen.
pub fn decide_reputation(ranked: bool, family_blocked: bool) -> Reputation {
    if family_blocked {
        Reputation::Adult
    } else if ranked {
        Reputation::Ranked
    } else {
        Reputation::Unranked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(body: &str) -> RobotsEntry {
        robots_entry(0, RobotsRules::parse(body.as_bytes(), "SlopNinjaBot"), ROBOTS_MAX_CACHE_MS)
    }

    #[test]
    fn one_fetch_at_a_time_and_the_sites_delays() {
        let mut s = HostState::default();
        let r = rules("User-agent: *\nCrawl-delay: 5\nDisallow: /private\n");
        let o = "https://a.example";
        assert_eq!(s.slot(10, o, "/x", None, 1000, "s1".into()), SlotStep::NeedRobots);
        let stale = RobotsEntry { until: 5, ..r.clone() };
        assert_eq!(s.slot(10, o, "/x", Some(&stale), 1000, "s1".into()), SlotStep::NeedRobots);
        assert_eq!(s.slot(10, o, "/private/a", Some(&r), 1000, "s1".into()), SlotStep::Answer(Slot::Disallowed));
        assert_eq!(
            s.slot(10, o, "/x", Some(&r), 1000, "s1".into()),
            SlotStep::Answer(Slot::Granted { slot: "s1".into() })
        );
        // A second fetch waits for the slot.
        assert_eq!(s.slot(20, o, "/y", Some(&r), 1000, "s2".into()), SlotStep::Answer(Slot::WaitUntil(1010)));
        // A stale id releases nothing.
        s.release(30, "s2", Some(&r), None);
        assert!(s.slot.is_some());
        // Released: the Crawl-delay holds the next one.
        s.release(30, "s1", Some(&r), None);
        assert_eq!(s.slot(31, o, "/y", Some(&r), 1000, "s2".into()), SlotStep::Answer(Slot::WaitUntil(5030)));
        assert_eq!(
            s.slot(5030, o, "/y", Some(&r), 1000, "s2".into()),
            SlotStep::Answer(Slot::Granted { slot: "s2".into() })
        );
        // A Retry-After longer than the delay wins.
        s.release(6000, "s2", Some(&r), Some(60_000));
        assert_eq!(s.next_at, 66_000);
        // A slot whose holder died frees itself at its lease.
        let mut t = HostState::default();
        let _ = t.slot(0, o, "/a", Some(&r), 100, "a".into());
        assert_eq!(
            t.slot(101, o, "/b", Some(&r), 100, "b".into()),
            SlotStep::Answer(Slot::Granted { slot: "b".into() })
        );
    }

    #[test]
    fn content_signals_refuse_as_robots_or_private_source() {
        let mut s = HostState::default();
        let no_search = rules("User-agent: *\nContent-Signal: search=no\nAllow: /\n");
        assert_eq!(
            s.slot(1, "https://a.example", "/", Some(&no_search), 10, "x".into()),
            SlotStep::Answer(Slot::NoSearch)
        );
        let no_ai = rules("User-agent: *\nContent-Signal: ai-input=no\nAllow: /\n");
        assert_eq!(
            s.slot(1, "https://a.example", "/", Some(&no_ai), 10, "x".into()),
            SlotStep::Answer(Slot::Disallowed)
        );
        assert!(s.slot.is_none());
    }

    #[test]
    fn robots_cache_at_most_a_day() {
        let e = robots_entry(100, RobotsRules::allow_all(), 7 * ROBOTS_MAX_CACHE_MS);
        assert_eq!(e.until, 100 + ROBOTS_MAX_CACHE_MS);
        assert_eq!(robots_entry(100, RobotsRules::allow_all(), 5).until, 105);
        assert_eq!(
            origin_and_path("https://a.example/p/q?x=1#f"),
            Some(("https://a.example".into(), "/p/q?x=1".into()))
        );
    }

    #[test]
    fn screens_cache_for_their_ttl_within_bounds() {
        let mut s = HostState::default();
        s.put_screen(0, "a.example", true, 0);
        assert_eq!(s.screen(59_999, "a.example"), Some(true));
        assert_eq!(s.screen(60_000, "a.example"), None);
        s.put_screen(0, "b.example", false, 10 * SCREEN_MAX_TTL_S);
        assert_eq!(s.screen(SCREEN_MAX_TTL_S * 1000 - 1, "b.example"), Some(false));
        for i in 0..(CACHE_ENTRIES + 10) {
            s.put_screen(1, &format!("h{i}.example"), false, 300 + i as u64);
        }
        assert!(s.screens.len() <= CACHE_ENTRIES);
    }

    #[test]
    fn doh_answers() {
        let blocked = br#"{"Status":0,"Answer":[{"name":"x.example","type":1,"TTL":30,"data":"0.0.0.0"}]}"#;
        assert_eq!(parse_doh(blocked), Some(DohAnswer { blocked: true, ttl_s: 30 }));
        let ok = br#"{"Status":0,"Answer":[{"type":5,"TTL":300,"data":"c.example."},{"type":1,"TTL":120,"data":"192.0.2.1"}]}"#;
        assert_eq!(parse_doh(ok), Some(DohAnswer { blocked: false, ttl_s: 120 }));
        assert_eq!(parse_doh(br#"{"Status":3}"#), Some(DohAnswer { blocked: false, ttl_s: SCREEN_MIN_TTL_S }));
        assert_eq!(parse_doh(br#"{"Status":2}"#), None);
        assert_eq!(parse_doh(b"<html>"), None);
        assert_eq!(
            doh_url(SECURITY_DOH, "a.example"),
            "https://security.cloudflare-dns.com/dns-query?name=a.example&type=A"
        );
        assert_eq!(decide_reputation(true, false), Reputation::Ranked);
        assert_eq!(decide_reputation(true, true), Reputation::Adult);
        assert_eq!(decide_reputation(false, false), Reputation::Unranked);
    }

    #[test]
    fn reputation_follows_the_list_version() {
        let mut s = HostState::default();
        s.put_reputation(0, Reputation::Ranked, "r-2026-11-09");
        assert_eq!(s.reputation(1, "r-2026-11-09"), Some(Reputation::Ranked));
        assert_eq!(s.reputation(1, "r-2026-11-16"), None);
        assert_eq!(s.reputation(REPUTATION_CACHE_MS, "r-2026-11-09"), None);
    }
}

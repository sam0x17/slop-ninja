//! The `DetectHost` Durable Object (wasm32): one per registrable domain,
//! named by it. Its rules are [`crate::host`]; this is their storage and
//! their outside calls. It never sees a page body.
//!
//! Routes (binding-only, `POST`, JSON):
//! - `/screen {host}` → `{risk}`: the security screen (DoH JSON to
//!   1.1.1.1 for Families' security resolver), cached for its TTL; 503 when
//!   the resolver cannot answer;
//! - `/slot {url, lease_ms}` → [`SlotOut`]: robots.txt first when the
//!   origin's cached rules are missing or stale (`DETECT_FETCH /robots`),
//!   then the politeness slot;
//! - `/release {slot, retry_after_ms}`: the fetch under `slot` ended;
//! - `/reputation {domain}` → `{reputation, list_v}`: the ranking shard (R2
//!   `rank/<v>/<xx>`) and the family screen, cached for a day.
//!
//! State: one storage value `state` ([`HostState`]) and one per origin
//! `robots:<origin>` (a large rule set needs its own value). Storage that
//! cannot be read starts from an empty state: the worst case is one more
//! robots.txt read or screen query.

use serde::{Deserialize, Serialize};
use slop_ninja_detect_fetch_wire::{FETCH_ORIGIN, ROBOTS_PATH, RobotsAnswer, RobotsRequest};
use slop_ninja_detect_page::codes::Reputation;
use worker::{DurableObject, Env, Method, Request, Response, Result, State, durable_object};

use crate::cf_io::{self, now_ms};
use crate::host::{self, FAMILY_DOH, HostState, RobotsEntry, SECURITY_DOH, SlotStep};
use crate::net::Slot;
use crate::reputation;
use crate::silence_panics;

/// The request bodies' bound.
const BODY_BYTES: usize = 16 * 1024;
/// A DoH answer's bound.
const DOH_BYTES: usize = 64 * 1024;
/// A robots answer's bound: the parsed rules of a 500 KiB file as JSON.
const ROBOTS_BYTES: usize = 4 * 1024 * 1024;
/// A rank shard's bound (about 4,000 domains of at most 253 bytes).
const SHARD_BYTES: usize = 2 * 1024 * 1024;
/// The wait of one outside call from the object.
const CALL_MS: u64 = 5_000;

#[durable_object]
pub struct DetectHost {
    state: State,
    env: Env,
}

#[derive(Deserialize)]
struct ScreenIn {
    host: String,
}

#[derive(Serialize)]
struct ScreenOut {
    risk: bool,
}

#[derive(Deserialize)]
struct SlotIn {
    url: String,
    lease_ms: u64,
}

/// A slot answer on the wire.
#[derive(Serialize, Deserialize)]
pub(crate) struct SlotOut {
    pub(crate) granted: Option<String>,
    pub(crate) disallowed: bool,
    pub(crate) no_search: bool,
    pub(crate) wait_until: Option<u64>,
}

impl SlotOut {
    pub(crate) fn of(s: &Slot) -> SlotOut {
        let mut o = SlotOut { granted: None, disallowed: false, no_search: false, wait_until: None };
        match s {
            Slot::Granted { slot } => o.granted = Some(slot.clone()),
            Slot::Disallowed => o.disallowed = true,
            Slot::NoSearch => o.no_search = true,
            Slot::WaitUntil(t) => o.wait_until = Some(*t),
        }
        o
    }

    pub(crate) fn slot(&self) -> Option<Slot> {
        Some(match (&self.granted, self.disallowed, self.no_search, self.wait_until) {
            (Some(g), false, false, None) => Slot::Granted { slot: g.clone() },
            (None, true, false, None) => Slot::Disallowed,
            (None, false, true, None) => Slot::NoSearch,
            (None, false, false, Some(t)) => Slot::WaitUntil(t),
            _ => return None,
        })
    }
}

#[derive(Deserialize)]
struct ReleaseIn {
    slot: String,
    retry_after_ms: Option<u64>,
}

#[derive(Deserialize)]
struct RepIn {
    domain: String,
}

#[derive(Serialize)]
struct RepOut {
    reputation: Reputation,
    list_v: String,
}

fn status(code: u16) -> Result<Response> {
    Response::empty().map(|r| r.with_status(code))
}

impl DetectHost {
    async fn load(&self) -> HostState {
        self.state.storage().get::<HostState>("state").await.ok().flatten().unwrap_or_default()
    }

    async fn save(&self, s: &HostState) {
        let _ = self.state.storage().put("state", s).await;
    }

    async fn robots(&self, origin: &str) -> Option<RobotsEntry> {
        self.state.storage().get::<RobotsEntry>(&format!("robots:{origin}")).await.ok().flatten()
    }

    /// One robots.txt read through the fetch Worker.
    async fn read_robots(&self, origin: &str) -> Option<RobotsEntry> {
        let fetcher = self.env.service(crate::cf_url::FETCH).ok()?;
        let body = serde_json::to_string(&RobotsRequest { origin: origin.to_owned(), wait_ms: CALL_MS }).ok()?;
        let req = cf_io::internal_request(
            &format!("{FETCH_ORIGIN}{ROBOTS_PATH}"),
            Method::Post,
            Some(&body),
            Some(CALL_MS * 2),
        )
        .ok()?;
        // On the request itself: a request built from a web_sys request is
        // immutable through `headers_mut`.
        req.inner().headers().set("Content-Type", "application/json").ok()?;
        let mut res = fetcher.fetch_request(req).await.ok()?;
        if res.status_code() != 200 {
            cf_io::discard(&mut res);
            return None;
        }
        let a: RobotsAnswer = cf_io::response_json(&mut res, ROBOTS_BYTES).await.ok()?;
        let e = host::robots_entry(now_ms(), a.rules, a.cache_ms);
        let _ = self.state.storage().put(&format!("robots:{origin}"), &e).await;
        Some(e)
    }

    /// One DoH JSON query.
    async fn doh(&self, endpoint: &str, name: &str) -> Option<host::DohAnswer> {
        let req = cf_io::internal_request(&host::doh_url(endpoint, name), Method::Get, None, Some(CALL_MS)).ok()?;
        // On the request itself (as above): without it the resolver does not
        // answer JSON, and the family screen would count every domain adult.
        req.inner().headers().set("Accept", "application/dns-json").ok()?;
        let mut res = worker::Fetch::Request(req).send().await.ok()?;
        if res.status_code() != 200 {
            cf_io::discard(&mut res);
            return None;
        }
        let body = cf_io::response_bytes(&mut res, DOH_BYTES).await.ok()?;
        host::parse_doh(&body)
    }

    /// Whether the domain is in the current ranking list, and the list's
    /// version (`none` with no list).
    async fn ranked(&self, domain: &str) -> (bool, String) {
        let Ok(bucket) = self.env.bucket(crate::cf_store::DOCS) else { return (false, reputation::NO_LIST.into()) };
        let until = Some(now_ms().saturating_add(CALL_MS));
        let Ok(Some(cur)) = cf_io::r2_bytes(&bucket, reputation::RANK_CURRENT, 256, until).await else {
            return (false, reputation::NO_LIST.into());
        };
        let Some(v) = reputation::current_version(&cur) else { return (false, reputation::NO_LIST.into()) };
        let key = reputation::shard_key(&v, reputation::shard_of(domain));
        match cf_io::r2_bytes(&bucket, &key, SHARD_BYTES, until).await {
            Ok(Some(body)) => (reputation::in_shard(&body, domain), v),
            _ => (false, v),
        }
    }
}

impl DurableObject for DetectHost {
    fn new(state: State, env: Env) -> Self {
        silence_panics();
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        silence_panics();
        if req.method() != Method::Post {
            return status(405);
        }
        let now = now_ms();
        match req.path().as_str() {
            "/screen" => {
                let Ok(i) = cf_io::request_json::<ScreenIn>(&mut req, BODY_BYTES).await else { return status(400) };
                let mut st = self.load().await;
                if let Some(risk) = st.screen(now, &i.host) {
                    return Response::from_json(&ScreenOut { risk });
                }
                let Some(a) = self.doh(SECURITY_DOH, &i.host).await else { return status(503) };
                st.put_screen(now, &i.host, a.blocked, a.ttl_s);
                self.save(&st).await;
                Response::from_json(&ScreenOut { risk: a.blocked })
            }
            "/slot" => {
                let Ok(i) = cf_io::request_json::<SlotIn>(&mut req, BODY_BYTES).await else { return status(400) };
                let Some((origin, path)) = host::origin_and_path(&i.url) else { return status(400) };
                let mut robots = self.robots(&origin).await;
                let mut st = self.load().await;
                let id = hex::encode(crate::cf::random_bytes(12).to_vec());
                let mut step = st.slot(now, &origin, &path, robots.as_ref(), i.lease_ms, id.clone());
                if step == SlotStep::NeedRobots {
                    robots = self.read_robots(&origin).await;
                    if robots.is_none() {
                        return status(503);
                    }
                    step = st.slot(now, &origin, &path, robots.as_ref(), i.lease_ms, id);
                }
                let SlotStep::Answer(slot) = step else { return status(503) };
                self.save(&st).await;
                Response::from_json(&SlotOut::of(&slot))
            }
            "/release" => {
                let Ok(i) = cf_io::request_json::<ReleaseIn>(&mut req, BODY_BYTES).await else { return status(400) };
                let mut st = self.load().await;
                let origin = st.slot.as_ref().map(|h| h.origin.clone());
                let robots = match origin {
                    Some(o) => self.robots(&o).await,
                    None => None,
                };
                st.release(now, &i.slot, robots.as_ref(), i.retry_after_ms);
                self.save(&st).await;
                status(204)
            }
            "/reputation" => {
                let Ok(i) = cf_io::request_json::<RepIn>(&mut req, BODY_BYTES).await else { return status(400) };
                let domain = i.domain;
                let mut st = self.load().await;
                let (ranked, v) = self.ranked(&domain).await;
                if let Some(r) = st.reputation(now, &v) {
                    return Response::from_json(&RepOut { reputation: r, list_v: v });
                }
                let family = self.doh(FAMILY_DOH, &domain).await;
                // A family screen that cannot answer counts as blocked: an
                // unknown site shows no title or excerpt until it answers.
                let blocked = family.is_none_or(|a| a.blocked);
                let r = host::decide_reputation(ranked, blocked);
                if family.is_some() {
                    st.put_reputation(now, r, &v);
                    self.save(&st).await;
                }
                Response::from_json(&RepOut { reputation: r, list_v: v })
            }
            _ => status(404),
        }
    }
}

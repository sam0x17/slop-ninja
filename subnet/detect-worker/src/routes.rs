//! The detect Worker's arena routes, footprints and the isolate's
//! scan memory. Compiled natively and for wasm32, so the numbers the tests check
//! against the memory model are the ones the Worker uses.
//!
//! **The scan memory.** Score requests run concurrently in one isolate, and
//! a request waiting in the queue keeps its text. So each isolate keeps one
//! fixed share of linear memory for scans, derived from the memory model,
//! and each score request reserves its worst-case footprint from it, sized
//! from its declared body length, in two steps. Before reading a byte of the
//! body it reserves only what the request holds until its envelope parses
//! (the body and the unescaped text, [`intake_footprint`]); once the envelope
//! parses it reserves the rest of [`scan_footprint`]. A body that arrives
//! slowly therefore holds 2 bytes of scan memory per declared byte, not the
//! whole footprint, and only until the body wait (`BODY_WAIT_MS`) ends. A
//! request that does not fit gets 503 "busy, try again" (unread at the first
//! step); one that could never fit, even alone, is refused as too large
//! before anything is read. The scan memory is a byte pool rather than a
//! count of worst-case arenas, so short texts do not each hold a 9 MB arena.
//!
//! **Forwarded requests** ([`forwarded`]): the `X-SN-*` allow-list, the
//! principal rule and the CSRF check, decided before any body is read.

use crate::bound;
use slop_ninja_arena::{AsyncRoute, limits, model};

pub mod id {
    pub const SCORE: u8 = 1;
    pub const WARM: u8 = 2;
    pub const FEEDBACK: u8 = 3;
    pub const OTHER: u8 = 4;
}

const KIB: usize = 1024;
const MIB: usize = 1024 * KIB;

/// The longest documented Workers wall limit stands in as the abandonment
/// deadline, as in the other Workers. A score request's wall time is its
/// queue wait plus a cold start plus one scan.
pub const FETCH_MS: u64 = limits::ALARM_MS;

pub const R_SCORE: AsyncRoute = AsyncRoute::new(id::SCORE, "detect/score", FETCH_MS);
pub const R_WARM: AsyncRoute = AsyncRoute::new(id::WARM, "detect/warm", FETCH_MS);
pub const R_FEEDBACK: AsyncRoute = AsyncRoute::new(id::FEEDBACK, "detect/feedback", FETCH_MS);
pub const R_OTHER: AsyncRoute = AsyncRoute::new(id::OTHER, "detect/other", FETCH_MS);

/// The scorer's reply for one document, at most: one sentence record per
/// content token at the most (a sentence holds at least one token) and one
/// span record per 24 words, each under 256 bytes of JSON, plus the
/// document prediction (origin, attribution) within 512 KiB; rounded up to
/// whole pages (1 MiB).
pub const REPLY_BYTES: usize = MIB;
const _: () = assert!(REPLY_BYTES >= bound::CONTENT_TOKENS as usize * 256 + 512 * KIB);

/// The fixed part of a score request: the reply, its parsed allow-list, the
/// answer line and the workers-rs request and response objects.
pub const SCORE_CONTROL_BYTES: usize = REPLY_BYTES + 512 * KIB;
/// A warm request: no body, one small JSON answer.
pub const WARM_CONTROL_BYTES: usize = 64 * KIB;
/// A feedback request: at most [`FEEDBACK_BODY_BYTES`] in, nothing out.
pub const FEEDBACK_CONTROL_BYTES: usize = 64 * KIB;
/// A 404: nothing beyond the route's own state.
pub const OTHER_CONTROL_BYTES: usize = slop_ninja_arena::PAGE;

/// A feedback body: `{"receipt":"<32 hex>.<band>.<bin>.<32 hex>","claim":"was_ai"}`.
pub const FEEDBACK_BODY_BYTES: usize = 256;

pub const MIN_CONTROL_BYTES: usize = OTHER_CONTROL_BYTES;
pub const LARGEST_INVOCATION_BYTES: usize = SCORE_CONTROL_BYTES + slop_ninja_arena::PAGE;

/// Bytes a score request can hold per byte of its body, at most: the body
/// (1), the unescaped text (at most 1: unescaping never lengthens), the
/// text's NFC copy inside the token count (3: NFC never more than triples
/// UTF-8), the count's symbol buffer and merge queue for its longest
/// pre-token (12 + 24 per byte, doubled as they grow: 72), and the body
/// forwarded to the scorer (6: minimal JSON escaping is at most `\u00XX`
/// per byte).
pub const BYTES_PER_BODY_BYTE: usize = 1 + 1 + 3 + 72 + 6;

/// Of those, the bytes a score request holds before its envelope parses:
/// the body and the unescaped text.
pub const INTAKE_BYTES_PER_BODY_BYTE: usize = 1 + 1;

/// The Durable Objects' share of an isolate they may share with the
/// Worker: the gate's ticket table ([`GATE_TABLE_BYTES`]), the feedback
/// ledger's used ids ([`FEEDBACK_TABLE_BYTES`]) and the lane's manifest and
/// replies.
pub const GATE_TABLE_BYTES: usize = 8 * MIB;
pub const FEEDBACK_TABLE_BYTES: usize = MIB;
pub const LANE_RESIDENT_BYTES: usize = REPLY_BYTES + 2 * MIB;

/// The feedback ledger's used-id bound, from its memory share. Past it the
/// ledger starts over with a new key (`receipt::Ledger::stale`).
pub const MAX_USED_IDS: usize = FEEDBACK_TABLE_BYTES / crate::receipt::USED_ID_BYTES;

/// Bytes one ticket holds in the gate: its waiting entry, its owner entry,
/// its turn in the round-robin order, the waiting ask's channel and its
/// waiter entry, and their B-tree, deque and allocator overhead, rounded up.
pub const TICKET_BYTES: usize = 256;
/// The gate's ticket table bound, from its memory share.
pub const MAX_TICKETS: usize = GATE_TABLE_BYTES / TICKET_BYTES;

/// The isolate's scan memory: the model's linear memory less the stack, the
/// glue partition, the objects' share and room for one invocation of every
/// other route at once per glue ticket's worth of them (bounded by the
/// largest non-score footprint times the glue tickets).
pub const SCAN_MEMORY_BYTES: usize = model::LINEAR_MEMORY_BYTES
    .saturating_sub(model::STACK_BYTES)
    .saturating_sub(model::GLUE_BUDGET_BYTES)
    .saturating_sub(GATE_TABLE_BYTES)
    .saturating_sub(FEEDBACK_TABLE_BYTES)
    .saturating_sub(LANE_RESIDENT_BYTES)
    .saturating_sub(WARM_CONTROL_BYTES * 16);

/// A score request's reservation for a declared body of `body_bytes`
/// (`None`: no declared length, so the bound).
pub fn scan_footprint(body_bytes: Option<u64>) -> usize {
    let n = body_bytes.unwrap_or(bound::RAW_BODY_BYTES).min(bound::RAW_BODY_BYTES);
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    n.saturating_mul(BYTES_PER_BODY_BYTE).saturating_add(SCORE_CONTROL_BYTES)
}

/// The part of [`scan_footprint`] a score request reserves before reading its
/// body: what it holds until its envelope parses.
pub fn intake_footprint(body_bytes: u64) -> usize {
    let n = body_bytes.min(bound::RAW_BODY_BYTES);
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    n.saturating_mul(INTAKE_BYTES_PER_BODY_BYTE)
}

/// The isolate's scan reservations (wasm32 isolates are single-threaded).
#[derive(Debug, Default)]
pub struct ScanMemory {
    held: std::cell::Cell<usize>,
}

/// Why a reservation was not granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoRoom {
    /// Others hold the room now: try again.
    Busy,
    /// Even alone this request would not fit.
    TooLarge,
}

impl ScanMemory {
    pub const fn new() -> ScanMemory {
        ScanMemory { held: std::cell::Cell::new(0) }
    }

    /// Takes `bytes` from the pool of scan memory; the caller gives them back with
    /// [`ScanMemory::give_back`] when its request ends (a guard's `Drop`).
    pub fn take(&self, bytes: usize) -> Result<(), NoRoom> {
        if bytes > SCAN_MEMORY_BYTES {
            return Err(NoRoom::TooLarge);
        }
        let held = self.held.get();
        if held.saturating_add(bytes) > SCAN_MEMORY_BYTES {
            return Err(NoRoom::Busy);
        }
        self.held.set(held + bytes);
        Ok(())
    }

    pub fn give_back(&self, bytes: usize) {
        self.held.set(self.held.get().saturating_sub(bytes));
    }

    pub fn held(&self) -> usize {
        self.held.get()
    }
}

/// The admission of a forwarded request: the `X-SN-*` allow-list, the
/// principal rule and the CSRF check (`auth-core::csrf`), all decided from
/// headers before a byte of the body is read.
///
/// - A request carrying `X-SN-Route` is a forwarded browser request. Its
///   route id must be the path's, its `X-SN-Host` must be one of the two
///   configured hosts' values and admitted by the route's table row, and
///   the CSRF check applies when the row's `csrf` column says so (Δ5: only
///   requests that carry `X-SN-Route`).
/// - `X-SN-Principal` is accepted only on a route whose row takes one, with
///   `X-SN-Host` the site's, and only in the exact form the website writes.
///   On any other route, or without `X-SN-Route`, its presence refuses the
///   request: a free check is the same request for everyone.
/// - A request with neither header is a direct binding call. It is served,
///   but its scans are never counted toward the pot ([`forwarded::Admission::channel`]).
///   `X-SN-Host` alone refuses it: the forwarders set both or neither.
/// - An `/internal/` request carrying `X-SN-Route` or `X-SN-Host` is refused
///   ([`forwarded::admit_internal`]): only the forwarders set those, and they never
///   forward an internal path.
pub mod forwarded {
    use slop_ninja_auth_core::csrf;
    use slop_ninja_detect_page::sn::{self, X_SN_HOST, X_SN_PRINCIPAL, X_SN_ROUTE};
    use slop_ninja_detect_page::{Channel, HostRole, Origins, Principal, RefusalCode, RouteId};

    /// Every request header this crate reads: the forwarders' allow-list.
    /// `tests/source.rs` holds each header read in the Worker's sources to
    /// it. (The shared body reader also reads `content-encoding`, to bound a
    /// compressed body.)
    pub const READ_HEADERS: &[&str] = &[
        "content-type",
        "content-length",
        "cf-connecting-ip",
        X_SN_HOST,
        X_SN_ROUTE,
        X_SN_PRINCIPAL,
        "origin",
        "sec-fetch-site",
    ];

    /// The headers the admission reads, as received.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Arrival<'a> {
        pub method: &'a str,
        pub sn_route: Option<&'a str>,
        pub sn_host: Option<&'a str>,
        pub sn_principal: Option<&'a str>,
        pub origin: Option<&'a str>,
        pub sec_fetch_site: Option<&'a str>,
    }

    /// How a request arrived.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Admission {
        /// Through a public forwarder, on `host`, for `principal` when the
        /// route takes one and the website sent it.
        Forwarded { host: HostRole, principal: Option<Principal> },
        /// A binding call that no forwarder made.
        Direct,
    }

    impl Admission {
        /// The counting channel of a free scan on `route`: `free_text` only
        /// for the score route forwarded from the site host. `None`: the scan
        /// is served and journaled as `uncovered_path`, never counted.
        pub fn channel(&self, route: RouteId) -> Option<Channel> {
            match (self, route) {
                (Admission::Forwarded { host: HostRole::Site, .. }, RouteId::Score) => Some(Channel::FreeText),
                _ => None,
            }
        }
    }

    /// The admission of a request to public route `route`.
    pub fn admit(route: RouteId, a: &Arrival<'_>, origins: &Origins) -> Result<Admission, RefusalCode> {
        let Some(id) = a.sn_route else {
            if a.sn_host.is_some() || a.sn_principal.is_some() {
                return Err(RefusalCode::MalformedRequest);
            }
            return Ok(Admission::Direct);
        };
        let id = RouteId::parse(id).filter(|r| *r == route).ok_or(RefusalCode::MalformedRequest)?;
        let host = a.sn_host.and_then(|h| origins.role_of_sn_host(h)).ok_or(RefusalCode::MalformedRequest)?;
        if !id.spec().hosts.admits(host) {
            return Err(RefusalCode::MalformedRequest);
        }
        let principal = match a.sn_principal {
            None => None,
            Some(p) => {
                if !id.takes_principal() || host != HostRole::Site {
                    return Err(RefusalCode::MalformedRequest);
                }
                Some(Principal::decode(p).ok_or(RefusalCode::MalformedRequest)?)
            }
        };
        csrf::check_route(id, host, origins, a.method, a.sec_fetch_site, a.origin).map_err(|e| e.code())?;
        Ok(Admission::Forwarded { host, principal })
    }

    /// The admission of an `/internal/` request.
    pub fn admit_internal(a: &Arrival<'_>) -> Result<(), RefusalCode> {
        if sn::internal_request_allowed(a.sn_route.is_some(), a.sn_host.is_some()) {
            Ok(())
        } else {
            Err(RefusalCode::MalformedRequest)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use slop_ninja_detect_page::UserId;

        const SITE: &str = "https://site.example";
        const SECRET: &str = "https://secret.example";

        fn origins() -> Origins {
            Origins::from_vars(SITE, SECRET).unwrap()
        }

        fn forwarded<'a>(route: &'a str, host: &'a str) -> Arrival<'a> {
            Arrival {
                method: "POST",
                sn_route: Some(route),
                sn_host: Some(host),
                sec_fetch_site: Some("same-origin"),
                origin: Some(SITE),
                ..Arrival::default()
            }
        }

        fn principal() -> String {
            Principal { user_id: UserId::parse("0123456789abcdef0123456789abcdef").unwrap(), auth_time_ms: 5, flags: 0 }
                .encode()
        }

        #[test]
        fn a_forwarded_score_from_the_site_is_counted() {
            let o = origins();
            let a = admit(RouteId::Score, &forwarded("score", "site.example"), &o).unwrap();
            assert_eq!(a, Admission::Forwarded { host: HostRole::Site, principal: None });
            assert_eq!(a.channel(RouteId::Score), Some(Channel::FreeText));
            assert_eq!(a.channel(RouteId::Warm), None);
        }

        #[test]
        fn a_direct_call_is_served_uncounted() {
            let a = admit(RouteId::Score, &Arrival { method: "POST", ..Arrival::default() }, &origins()).unwrap();
            assert_eq!(a, Admission::Direct);
            assert_eq!(a.channel(RouteId::Score), None);
        }

        #[test]
        fn the_route_and_host_must_be_the_forwarders() {
            let o = origins();
            for (route, host) in [
                ("warm", "site.example"),
                ("nope", "site.example"),
                ("", "site.example"),
                ("Score", "site.example"),
                ("score", "secret.example"),
                ("score", "other.example"),
                ("score", "https://site.example"),
                ("score", "SITE.EXAMPLE"),
                ("score", ""),
            ] {
                assert_eq!(
                    admit(RouteId::Score, &forwarded(route, host), &o),
                    Err(RefusalCode::MalformedRequest),
                    "{route} {host}"
                );
            }
            let no_host = Arrival { sn_host: None, ..forwarded("score", "site.example") };
            assert_eq!(admit(RouteId::Score, &no_host, &o), Err(RefusalCode::MalformedRequest));
            let host_only = Arrival { method: "POST", sn_host: Some("site.example"), ..Arrival::default() };
            assert_eq!(admit(RouteId::Score, &host_only, &o), Err(RefusalCode::MalformedRequest));
            // Feedback is admitted on both hosts, against each host's origin.
            let secret = Arrival { origin: Some(SECRET), ..forwarded("feedback", "secret.example") };
            assert!(admit(RouteId::Feedback, &secret, &o).is_ok());
            assert_eq!(
                admit(RouteId::Feedback, &forwarded("feedback", "secret.example"), &o),
                Err(RefusalCode::Csrf),
                "the secret host's request carries the site's origin"
            );
        }

        #[test]
        fn a_principal_only_where_the_table_takes_one() {
            let o = origins();
            let p = principal();
            for route in ["score", "warm", "feedback"] {
                let r = RouteId::parse(route).unwrap();
                let a = Arrival { sn_principal: Some(&p), ..forwarded(route, "site.example") };
                assert_eq!(admit(r, &a, &o), Err(RefusalCode::MalformedRequest), "{route}");
            }
            let direct = Arrival { method: "POST", sn_principal: Some(&p), ..Arrival::default() };
            assert_eq!(admit(RouteId::Score, &direct, &o), Err(RefusalCode::MalformedRequest));
            // `share` takes one from the site host, in the exact form only.
            let a = Arrival { sn_principal: Some(&p), ..forwarded("share", "site.example") };
            let Ok(Admission::Forwarded { principal: Some(got), .. }) = admit(RouteId::Share, &a, &o) else { panic!() };
            assert_eq!(got.auth_time_ms, 5);
            for bad in
                ["v1;0123;5;0", "v2;0123456789abcdef0123456789abcdef;5;0", "v1;0123456789abcdef0123456789abcdef;05;0"]
            {
                let a = Arrival { sn_principal: Some(bad), ..forwarded("share", "site.example") };
                assert_eq!(admit(RouteId::Share, &a, &o), Err(RefusalCode::MalformedRequest), "{bad}");
            }
            assert_eq!(
                admit(RouteId::Share, &forwarded("share", "site.example"), &o).unwrap(),
                Admission::Forwarded { host: HostRole::Site, principal: None }
            );
        }

        #[test]
        fn the_csrf_check_applies_to_forwarded_routes_only() {
            let o = origins();
            let cross = Arrival { sec_fetch_site: Some("cross-site"), ..forwarded("score", "site.example") };
            assert_eq!(admit(RouteId::Score, &cross, &o), Err(RefusalCode::Csrf));
            let same_site = Arrival {
                sec_fetch_site: Some("same-site"),
                origin: Some(SECRET),
                ..forwarded("score", "site.example")
            };
            assert_eq!(admit(RouteId::Score, &same_site, &o), Err(RefusalCode::Csrf), "secret host to site");
            let null = Arrival { sec_fetch_site: None, origin: Some("null"), ..forwarded("score", "site.example") };
            assert_eq!(admit(RouteId::Score, &null, &o), Err(RefusalCode::Csrf));
            let missing = Arrival { sec_fetch_site: None, origin: None, ..forwarded("score", "site.example") };
            assert_eq!(admit(RouteId::Score, &missing, &o), Err(RefusalCode::Csrf));
            let origin_only = Arrival { sec_fetch_site: None, ..forwarded("score", "site.example") };
            assert!(admit(RouteId::Score, &origin_only, &o).is_ok());
            let put = Arrival { method: "PUT", ..forwarded("score", "site.example") };
            assert_eq!(admit(RouteId::Score, &put, &o), Err(RefusalCode::Method));
            // A binding call carries no browser headers and gets no check.
            let direct = Arrival { method: "POST", sec_fetch_site: Some("cross-site"), ..Arrival::default() };
            assert_eq!(admit(RouteId::Score, &direct, &o), Ok(Admission::Direct));
        }

        #[test]
        fn internal_requests_never_carry_forwarder_headers() {
            assert!(admit_internal(&Arrival { method: "POST", ..Arrival::default() }).is_ok());
            for a in [
                Arrival { method: "POST", sn_route: Some("score"), ..Arrival::default() },
                Arrival { method: "POST", sn_host: Some("site.example"), ..Arrival::default() },
                forwarded("score", "site.example"),
            ] {
                assert_eq!(admit_internal(&a), Err(RefusalCode::MalformedRequest));
            }
        }

        #[test]
        fn the_read_list_holds_no_cookie() {
            assert!(!READ_HEADERS.contains(&"cookie"));
            assert!(READ_HEADERS.iter().all(|h| h.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_matches_the_route_table() {
        assert_eq!(model::MIN_ROUTE_BYTES, MIN_CONTROL_BYTES);
        assert_eq!(model::LARGEST_ROUTE_BYTES, LARGEST_INVOCATION_BYTES);
        const { assert!(FEEDBACK_BODY_BYTES + 4 * KIB < FEEDBACK_CONTROL_BYTES) };
        const { assert!(model::LARGEST_ROUTE_BYTES < model::LINEAR_MEMORY_BYTES) };
    }

    #[test]
    fn the_scan_memory_holds_ordinary_texts_side_by_side() {
        // A 1,500-word paste is about 10 KB of JSON.
        let one = scan_footprint(Some(10_000));
        assert!(SCAN_MEMORY_BYTES / one >= 32, "scan memory {SCAN_MEMORY_BYTES}, one {one}");
        // No declared length reserves the bound, which no isolate holds.
        assert!(scan_footprint(None) > SCAN_MEMORY_BYTES);
    }

    #[test]
    fn a_body_before_its_envelope_parses_holds_only_its_intake() {
        // The largest body that could fit alone.
        let declared = ((SCAN_MEMORY_BYTES - SCORE_CONTROL_BYTES) / BYTES_PER_BODY_BYTE) as u64;
        let (intake, full) = (intake_footprint(declared), scan_footprint(Some(declared)));
        assert!(full <= SCAN_MEMORY_BYTES && intake < full);
        // Slow bodies of that size, still arriving: about forty of them fit
        // side by side, where one full footprint takes the whole scan memory.
        assert!(SCAN_MEMORY_BYTES / intake >= 40, "intake {intake}, scan memory {SCAN_MEMORY_BYTES}");
        // Two steps reserve exactly the full footprint.
        let b = ScanMemory::new();
        b.take(intake).unwrap();
        b.take(full - intake).unwrap();
        assert_eq!(b.held(), full);
        b.give_back(full);
        assert_eq!(b.held(), 0);
    }

    #[test]
    fn the_feedback_ledger_fits_its_share() {
        const { assert!(MAX_USED_IDS * crate::receipt::USED_ID_BYTES <= FEEDBACK_TABLE_BYTES) };
        const { assert!(MAX_USED_IDS >= 16_000) };
    }

    #[test]
    fn reservations_return_their_bytes() {
        let b = ScanMemory::new();
        let big = SCAN_MEMORY_BYTES / 2 + 1;
        b.take(big).unwrap();
        assert_eq!(b.take(big).unwrap_err(), NoRoom::Busy);
        assert_eq!(b.take(SCAN_MEMORY_BYTES + 1).unwrap_err(), NoRoom::TooLarge);
        b.give_back(big);
        assert_eq!(b.held(), 0);
        assert!(b.take(big).is_ok());
    }
}

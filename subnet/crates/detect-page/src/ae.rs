//! The Analytics Engine point schema (content-free, written server-side).
//!
//! Each point is written with its name as the index and as the first blob,
//! followed by its fields in the order listed, so every point is queryable
//! by name and position. A point never carries a secret slug, a token, an
//! email address, a user id, the URL of a failed fetch or anything derived
//! from text. Public slugs may appear on `web` `view` points of public pages
//! only.

/// One point: its name and its blob fields, in order (after the name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub name: &'static str,
    pub blobs: &'static [&'static str],
}

/// The website and secret host's dataset (binding `WEB_METRICS`).
pub mod web {
    use super::Point;
    pub const VIEW: Point =
        Point { name: "view", blobs: &["route_class", "kind", "state", "band", "cache", "referrer_class", "status"] };
    pub const CARD: Point = Point { name: "card", blobs: &["variant", "unfurler_family"] };
    pub const ERROR: Point = Point { name: "error", blobs: &["route_class", "code"] };
    pub const ALL: &[Point] = &[VIEW, CARD, ERROR];
    /// `cache` values.
    pub const CACHE: &[&str] = &["hit", "miss"];
    /// `referrer_class` values.
    pub const REFERRER_CLASS: &[&str] = &["search", "social", "direct", "other"];
}

/// The detect Worker's dataset (binding `DETECT_METRICS`).
pub mod detect {
    use super::Point;
    /// Refusals before a ticket are counted here only, never in the pot
    /// counters.
    pub const SUBMIT: Point = Point { name: "submit", blobs: &["route", "outcome"] };
    /// `class` is a ticket class; the double is the service time in ms.
    pub const SCAN: Point = Point { name: "scan", blobs: &["class", "length_bin", "band", "ms_bucket"] };
    pub const QUEUE: Point = Point { name: "queue", blobs: &["depth_bucket"] };
    pub const FETCH: Point = Point { name: "fetch", blobs: &["outcome"] };
    pub const EXTRACT: Point = Point { name: "extract", blobs: &["outcome"] };
    /// `kind`: `claim_join`, `content` or `alias`.
    pub const REUSE: Point = Point { name: "reuse", blobs: &["kind"] };
    pub const STORE: Point = Point { name: "store", blobs: &["op"] };
    pub const JOB: Point = Point { name: "job", blobs: &["event"] };
    pub const REMOVAL: Point = Point { name: "removal", blobs: &["kind", "transition"] };
    pub const MANAGE: Point = Point { name: "manage", blobs: &["action"] };
    /// An owner action matched no row (`skipped`).
    pub const OWNER: Point = Point { name: "owner", blobs: &["result"] };
    /// `code` includes `d1` when the store could not be read.
    pub const ERROR: Point = Point { name: "error", blobs: &["code"] };
    pub const ALL: &[Point] = &[SUBMIT, SCAN, QUEUE, FETCH, EXTRACT, REUSE, STORE, JOB, REMOVAL, MANAGE, OWNER, ERROR];
    /// `reuse` kinds.
    pub const REUSE_KINDS: &[&str] = &["claim_join", "content", "alias"];
}

/// The account Worker's dataset (binding `ACCOUNT_METRICS`).
pub mod account {
    use super::Point;
    /// `step`: `start`, `sent`, `redeemed` or `failed`; `reason` a refusal
    /// code or empty.
    pub const AUTH: Point = Point { name: "auth", blobs: &["method", "step", "reason"] };
    /// The Email Sending result code.
    pub const MAIL: Point = Point { name: "mail", blobs: &["result_code"] };
    pub const SESSION: Point = Point { name: "session", blobs: &["event"] };
    pub const LEDGER: Point = Point { name: "ledger", blobs: &["event"] };
    /// An alert the poller raises (one of [`ALERTS`]).
    pub const ALERT: Point = Point { name: "alert", blobs: &["rule"] };
    pub const ERROR: Point = Point { name: "error", blobs: &["code"] };
    pub const ALL: &[Point] = &[AUTH, MAIL, SESSION, LEDGER, ALERT, ERROR];
    /// `auth` steps.
    pub const AUTH_STEPS: &[&str] = &["start", "sent", "redeemed", "failed"];
    /// An address reached `email_ceiling` (the send that emptied the bucket).
    pub const AUTH_MAIL_CEILING: &str = "auth_mail_ceiling";
    /// Email Sending's rate or daily limit refused a send.
    pub const AUTH_MAIL_QUOTA: &str = "auth_mail_quota";
    /// A connect request emptied an address's `email_link` bucket.
    pub const AUTH_LINK_CEILING: &str = "auth_link_ceiling";
    /// X refused a sign-in for credits or limits.
    pub const X_REFUSED: &str = "x_refused";
    pub const ALERTS: &[&str] = &[AUTH_MAIL_CEILING, AUTH_MAIL_QUOTA, AUTH_LINK_CEILING, X_REFUSED];
}

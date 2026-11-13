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
    /// One read of a post through X's API (one of [`XREAD_OUTCOMES`]). It
    /// carries no post id.
    pub const XREAD: Point = Point { name: "xread", blobs: &["outcome"] };
    /// One read of a post's embed markup (one of [`XEMBED_OUTCOMES`]). It
    /// carries no post id.
    pub const XEMBED: Point = Point { name: "xembed", blobs: &["outcome"] };
    pub const ALL: &[Point] =
        &[SUBMIT, SCAN, QUEUE, FETCH, EXTRACT, REUSE, STORE, JOB, REMOVAL, MANAGE, OWNER, ERROR, XREAD, XEMBED];
    /// `reuse` kinds.
    pub const REUSE_KINDS: &[&str] = &["claim_join", "content", "alias"];
    /// `xread` outcomes: `post` (X returned the post; each one counts toward
    /// the month's read limit), `unavailable_post` (not found, protected or
    /// suspended), `rate_limited` (X asked us to wait), `quota` (X has no
    /// reads left for the app), `token` (X refused the bearer token),
    /// `limit` (the month's `X_READ_BUDGET` was reached, so no request was
    /// made), `bad_answer` (over the bound or out of form) and `failed`
    /// (transport or timeout).
    pub const XREAD_OUTCOMES: &[&str] =
        &["post", "unavailable_post", "rate_limited", "quota", "token", "limit", "bad_answer", "failed"];
    /// `xembed` outcomes: `ok`, `gone` (X answered that the post is gone:
    /// the page ends with `x_gone`), `invalid` (markup outside the allowed
    /// shape or a link off X) and `failed` (transport, timeout or any other
    /// answer: the page is served `no-store`).
    pub const XEMBED_OUTCOMES: &[&str] = &["ok", "gone", "invalid", "failed"];
    /// The month's X reads neared `X_READ_BUDGET` (at the poller's
    /// threshold).
    pub const X_READ_BUDGET: &str = "x_read_budget";
    /// Alerts the poller raises from this dataset and the `status` op.
    pub const ALERTS: &[&str] = &[X_READ_BUDGET];
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

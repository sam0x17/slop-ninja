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
    /// One lookup of a post through X's API (`/x/post`), or one refused
    /// before it (one of [`XREAD_OUTCOMES`]). It carries no post id.
    pub const XREAD: Point = Point { name: "xread", blobs: &["outcome"] };
    /// One existence check of a post through oEmbed (`/x/check`), or one
    /// message's verdict on it (one of [`XCHECK_OUTCOMES`]). It carries no
    /// post id.
    pub const XCHECK: Point = Point { name: "xcheck", blobs: &["outcome"] };
    /// One page of the X sweep (`step` is `first` for the cron's message and
    /// `next` for a follow-up). The double is the age in seconds of the
    /// oldest `fetched_at` among X sources that hold a `canon_url` (0 when
    /// there is none), from which the poller raises [`X_SWEEP_LATE`].
    pub const XSWEEP: Point = Point { name: "xsweep", blobs: &["step"] };
    pub const ALL: &[Point] =
        &[SUBMIT, SCAN, QUEUE, FETCH, EXTRACT, REUSE, STORE, JOB, REMOVAL, MANAGE, OWNER, ERROR, XREAD, XCHECK, XSWEEP];
    /// `reuse` kinds.
    pub const REUSE_KINDS: &[&str] = &["claim_join", "content", "alias"];
    /// `xread` outcomes, read from X's problem `type` first and the HTTP
    /// status second:
    /// - `scored`: X returned the post, last in its edit history (it keeps
    ///   its reserved unit);
    /// - `superseded`: X returned an earlier version of an edited post
    ///   (`x_post_superseded`);
    /// - `not_found`: `resource-not-found` or `not-authorized-for-resource`
    ///   (`x_post_unavailable`; not billed, so the unit is released);
    /// - `rate_limited`: `rate-limit-exceeded`, or a 429 with no
    ///   `usage-capped` type (`x_busy`);
    /// - `usage_capped`: `usage-capped` at any status (`x_unavailable`,
    ///   never `x_busy`);
    /// - `credits`: X refused the read for credits (`x_unavailable`);
    /// - `token`: 401, or 403 with `client-forbidden` or no type
    ///   (`x_unavailable`);
    /// - `failed`: a 5xx, a timeout or a network error (`x_busy`);
    /// - `invalid`: any other answer, a body past `X_POST_BOUND`, JSON that
    ///   does not parse, a bad range, history or `editable_until`
    ///   (`x_unavailable`);
    /// - `prefix_mismatch`: written beside `scored` when a long-post body
    ///   does not begin with the leading reply-mention run and is kept whole;
    /// - `budget`: the billing cycle's `X_READ_BUDGET` was reached, so no
    ///   request was made (`x_unavailable`).
    pub const XREAD_OUTCOMES: &[&str] = &[
        "scored",
        "superseded",
        "not_found",
        "rate_limited",
        "usage_capped",
        "credits",
        "token",
        "failed",
        "invalid",
        "prefix_mismatch",
        "budget",
    ];
    /// `xcheck` outcomes:
    /// - `present`: HTTP 200 and an oEmbed object of type `rich` with an
    ///   `html` string (not read);
    /// - `gone_candidate`: a 404 or 403 whose body is oEmbed's own JSON error;
    ///   only this can lead to a deletion, once a lookup confirms it;
    /// - `unexpected`: any other answer (HTML, another status, another JSON
    ///   shape); it starts the failure hold;
    /// - `failed`: a timeout, a 5xx or a network error;
    /// - `contradicted`: a lookup found the post that oEmbed called a
    ///   candidate (the source is marked and oEmbed distrust starts);
    /// - `control`: the message's control post did not answer `present` (or
    ///   `X_CONTROL_ID` is missing), so every answer in it is `unexpected`.
    pub const XCHECK_OUTCOMES: &[&str] =
        &["present", "gone_candidate", "unexpected", "failed", "contradicted", "control"];
    /// `xsweep` steps.
    pub const XSWEEP_STEPS: &[&str] = &["first", "next"];
    /// The billing cycle's X reads neared `X_READ_BUDGET` (at the poller's
    /// threshold).
    pub const X_READ_BUDGET: &str = "x_read_budget";
    /// Checking posts from X is unavailable: `xread` `usage_capped`,
    /// `credits` or `token`.
    pub const X_UNAVAILABLE: &str = "x_unavailable";
    /// Any `xcheck` `unexpected` or `control` point: X may have changed
    /// oEmbed, which would stop the existence checks.
    pub const X_CHECK_UNEXPECTED: &str = "x_check_unexpected";
    /// Any `xcheck` `contradicted` point: oEmbed called a post gone that the
    /// lookup found (oEmbed distrust holds until `x_oembed_trust`).
    pub const X_CHECK_CONTRADICTED: &str = "x_check_contradicted";
    /// The oldest check of an X source holding a `canon_url` is more than 24
    /// hours old ([`XSWEEP`], `status`).
    pub const X_SWEEP_LATE: &str = "x_sweep_late";
    /// Alerts the poller raises from this dataset and the `status` op.
    pub const ALERTS: &[&str] = &[X_READ_BUDGET, X_UNAVAILABLE, X_CHECK_UNEXPECTED, X_CHECK_CONTRADICTED, X_SWEEP_LATE];
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

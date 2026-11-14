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
    /// A page view. A public page's slug may be its index; the `view` point
    /// of a page about a post on X (`Page::is_x_post`, on `/d/<7>` or
    /// `/d/<7>/x`) carries no slug, in any blob or as its index, and its band
    /// stays only as a count with no key, because AE points cannot be
    /// deleted and would outlive the page's removal.
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
    /// One check of a post from its link through X's free embed
    /// (`/x/post`), or one refused before any request (one of
    /// [`XREAD_OUTCOMES`]). It carries no post id and no slug.
    pub const XREAD: Point = Point { name: "xread", blobs: &["outcome"] };
    /// The existence check's answers in one sweep message: one point per
    /// outcome that occurred (one of [`XCHECK_OUTCOMES`]), whose double is
    /// the number of ids with that outcome (1 for `control`). A message
    /// writes at most five, whatever its batch size, because Analytics Engine
    /// keeps at most 250 points per invocation; the poller reads the counts.
    /// It carries no post id and no slug.
    pub const XCHECK: Point = Point { name: "xcheck", blobs: &["outcome"] };
    /// One sweep message (`step` is `first` for the cron's message and
    /// `next` for a follow-up). The double is the age in seconds of the
    /// oldest `fetched_at` among X sources that hold a `canon_url` (a NULL
    /// read as 0; 0 when there is none), from which the poller raises
    /// [`X_SWEEP_LATE`].
    pub const XSWEEP: Point = Point { name: "xsweep", blobs: &["step"] };
    pub const ALL: &[Point] =
        &[SUBMIT, SCAN, QUEUE, FETCH, EXTRACT, REUSE, STORE, JOB, REMOVAL, MANAGE, OWNER, ERROR, XREAD, XCHECK, XSWEEP];
    /// `reuse` kinds.
    pub const REUSE_KINDS: &[&str] = &["claim_join", "content", "alias"];
    /// `xread` outcomes:
    /// - `text`: the embed showed the post in full and its text was scored
    ///   (or refused by the floor or the English screen);
    /// - `long`: the embed cut the post (`x_post_long`);
    /// - `too_new`: refused before any request, inside the edit window
    ///   (`x_post_too_new`);
    /// - `unavailable`: the embed's own JSON error, 404 or 403
    ///   (`x_post_unavailable`);
    /// - `busy`: a 429, a 5xx, a timeout or a network error (`x_busy`);
    /// - `unexpected`: any other answer (`x_unavailable`); raises
    ///   [`X_CHECK_UNEXPECTED`].
    pub const XREAD_OUTCOMES: &[&str] = &["text", "long", "too_new", "unavailable", "busy", "unexpected"];
    /// `xcheck` outcomes:
    /// - `present`: HTTP 200 and an oEmbed object of type `rich` with an
    ///   `html` string (not read): `fetched_at` moves;
    /// - `gone`: a 404 or 403 whose body is oEmbed's own JSON error: the
    ///   source is deleted (`x_gone`);
    /// - `unexpected`: any other answer (HTML, another status or JSON shape,
    ///   a body past the bound): the source keeps its claim;
    /// - `failed`: a timeout, a 429, a 5xx or a network error: the source
    ///   keeps its claim;
    /// - `not_checked`: no request was made for it (the route stopped after an
    ///   `unexpected` or `failed` answer): its claim is cleared;
    /// - `control`: one of the message's two controls failed: the live
    ///   control (`X_CONTROL_ID`) did not answer `present`, or the gone
    ///   control (`X_GONE_CONTROL_ID`) did not answer `gone`, or either id is
    ///   missing or a placeholder. Nothing is recorded, and the chain stops.
    pub const XCHECK_OUTCOMES: &[&str] = &["present", "gone", "unexpected", "failed", "not_checked", "control"];
    /// `xsweep` steps.
    pub const XSWEEP_STEPS: &[&str] = &["first", "next"];
    /// A count above zero, summed from the doubles of the `xcheck`
    /// `unexpected` and `control` points, or any `xread` `unexpected` point:
    /// X may have changed or cached its embed's answers, the live control may
    /// be gone, or the gone control answers `present`; the existence checks
    /// stop until it is fixed.
    pub const X_CHECK_UNEXPECTED: &str = "x_check_unexpected";
    /// The oldest check of an X source holding a `canon_url` is more than 24
    /// hours old ([`XSWEEP`], `status`).
    pub const X_SWEEP_LATE: &str = "x_sweep_late";
    /// Alerts the poller raises from this dataset and the `status` op.
    pub const ALERTS: &[&str] = &[X_CHECK_UNEXPECTED, X_SWEEP_LATE];
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

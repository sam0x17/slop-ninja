//! Every refusal code a SlopNinja Worker emits, with its HTTP status and the
//! key of its one line of copy.
//!
//! The templates hold one line of copy per code, under [`RefusalCode::copy_key`],
//! and a test there checks that every code here has one. API errors are one
//! JSON object, [`ApiRefusal`]; form endpoints answer 303 to their page with
//! `?e=<code>`. Where the design fixes the copy, the variant's documentation
//! quotes it.

use serde::{Deserialize, Serialize};

use crate::codes::CutKind;
use crate::x::PostUrl;

macro_rules! refusal_codes {
    ($($(#[$vm:meta])* $var:ident = $s:literal, $status:literal, $group:ident;)+) => {
        /// A refusal code. Serializes as its string.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum RefusalCode { $($(#[$vm])* $var,)+ }

        impl RefusalCode {
            /// Every code, in declaration order.
            pub const ALL: &'static [RefusalCode] = &[$(RefusalCode::$var,)+];

            /// The wire string.
            pub const fn as_str(self) -> &'static str {
                match self { $(RefusalCode::$var => $s,)+ }
            }

            /// The code of a wire string.
            pub fn parse(s: &str) -> Option<RefusalCode> {
                match s { $($s => Some(RefusalCode::$var),)+ _ => None }
            }

            /// The HTTP status of an API refusal with this code.
            pub const fn status(self) -> u16 {
                match self { $(RefusalCode::$var => $status,)+ }
            }

            /// The key of the code's line of copy in the templates.
            pub const fn copy_key(self) -> &'static str {
                match self { $(RefusalCode::$var => concat!("refusal.", $s),)+ }
            }

            /// Which surface emits the code.
            pub const fn group(self) -> Group {
                match self { $(RefusalCode::$var => Group::$group,)+ }
            }
        }
    };
}

/// The surface a refusal belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    /// Any route: request shape, transport, CSRF, configuration.
    Request,
    /// The free check (`/score`) and its stream.
    Check,
    /// Seals, links and the store switches.
    Store,
    /// The re-run route.
    Rerun,
    /// The public URL check.
    Url,
    /// Posts on X: the free check's answer to a post link, the URL route's X
    /// adapter, and why an X page ended.
    X,
    /// Page reads, manage and owner actions.
    Page,
    /// Paid checks and the ledger.
    Paid,
    /// Removal requests.
    Removal,
    /// The signed op routes.
    Op,
    /// Sign-in: the `?e=` codes of `/signin/` and the auth APIs.
    Signin,
    /// Account actions.
    Account,
}

refusal_codes! {
    // ---------------------------------------------------------------- request
    /// The body is not the route's envelope.
    MalformedRequest = "malformed_request", 400, Request;
    /// The content type is not the route's (JSON or form).
    ContentType = "content_type", 415, Request;
    /// No declared length.
    LengthRequired = "length_required", 411, Request;
    /// "This text is too large for one check. Score a section instead."
    TooLarge = "too_large", 413, Request;
    /// The body did not arrive within its deadline, or broke off.
    Body = "body", 408, Request;
    /// Fetch Metadata or `Origin` failed the CSRF rule.
    Csrf = "csrf", 403, Request;
    NotFound = "not_found", 404, Request;
    Method = "method", 405, Request;
    Internal = "internal", 500, Request;
    /// A var or binding the route needs is missing.
    NotConfigured = "not_configured", 503, Request;
    /// A strict per-key quota is spent.
    Quota = "quota", 429, Request;

    // ------------------------------------------------------------- free check
    Empty = "empty", 422, Check;
    /// "This text is 17 words; the detector needs at least 24." (carries
    /// `words` and `min_words`).
    TooShort = "too_short", 422, Check;
    /// Over the scorer's token limit (carries `tokens`).
    TooLong = "too_long", 422, Check;
    NoWords = "no_words", 422, Check;
    /// "We couldn't find a sentence end in the first 1,000 words, so there is
    /// nothing to score."
    NoSentenceEnd = "no_sentence_end", 422, Check;
    /// "This doesn't look like English. The detector reads English only."
    NotEnglish = "not_english", 422, Check;
    /// "The text contains characters we can't read. Copy it again from the
    /// source." (an unpaired surrogate or control bytes; the built Worker's
    /// `invalid_text`).
    BadText = "bad_text", 400, Check;
    /// "The detector is busy. Try again in a minute."
    Busy = "busy", 503, Check;
    /// "The detector is restarting. Try again in a minute."
    Unavailable = "unavailable", 503, Check;
    /// As `unavailable`.
    Restarting = "restarting", 503, Check;
    /// "Your place in line expired. Check again." (the built Worker's
    /// `expired` stream line).
    ExpiredPlace = "expired_place", 503, Check;
    /// "Checks are paused right now."
    ToolOff = "tool_off", 503, Check;
    /// The scorer's reply was not usable.
    Scorer = "scorer", 502, Check;

    // ------------------------------------------------------------------ store
    /// "This result is more than a day old. Check it again to share it." (a
    /// forged, expired or mismatched seal).
    SealInvalid = "seal_invalid", 400, Store;
    /// The seal already made a link.
    SealUsed = "seal_used", 409, Store;
    /// "Saving is unavailable right now." (`DETECT_STORE` off).
    StoreOff = "store_off", 503, Store;
    /// Every stored page is temporarily unavailable (`DETECT_PAGES` off).
    PagesOff = "pages_off", 503, Store;

    // ------------------------------------------------------------------ rerun
    /// "This secret link was deleted or has expired, so it can't be checked
    /// again."
    RerunGone = "rerun_gone", 410, Rerun;
    /// "This page is unavailable, so it can't be checked again."
    RerunBlocked = "rerun_blocked", 451, Rerun;
    /// "This page can't be checked again."
    RerunUnavailable = "rerun_unavailable", 409, Rerun;
    /// "This page already uses the current model."
    RerunNotNeeded = "rerun_not_needed", 409, Rerun;

    // -------------------------------------------------------------------- url
    /// URL checks are switched off (`DETECT_URLS`).
    UrlsOff = "urls_off", 503, Url;
    /// The URL failed the input checks (IP literal, port, userinfo, reserved
    /// or own host).
    BadUrl = "bad_url", 422, Url;
    /// "We can't make a public page for this link: it is a private share
    /// link, or the page asks search engines not to list it. Paste its text to
    /// make a secret link."
    CapabilityUrl = "capability_url", 422, Url;
    /// As `capability_url`.
    PrivateSource = "private_source", 422, Url;
    /// "We don't check pages on sites listed as malware or phishing."
    SecurityRisk = "security_risk", 422, Url;
    /// The site's robots.txt disallows SlopNinjaBot. With every fetch failure
    /// below: "You can paste its text to check it privately."
    Robots = "robots", 422, Url;
    LoginWall = "login_wall", 422, Url;
    FetchFailed = "fetch_failed", 422, Url;
    TooSlow = "too_slow", 422, Url;
    UnsupportedType = "unsupported_type", 422, Url;
    PageTooLarge = "page_too_large", 422, Url;

    // ---------------------------------------------------------------------- x
    /// The free check's input is only an X post's link (rule `x_post_v1`):
    /// nothing is scored and no ticket is taken. Carries `url`, the post's
    /// canonical URL ([`ApiRefusal::x_post_link`]); the tool page posts it
    /// to the URL check and streams that. Sent only while `DETECT_URLS` and
    /// `DETECT_X` are both on.
    XPostLink = "x_post_link", 422, X;
    /// "Checking a post from its link isn't available right now. Paste the
    /// post's text to check it." (`DETECT_URLS` or `DETECT_X` is off, or
    /// `DETECT_X` is `purged`.)
    XOff = "x_off", 503, X;
    /// A link on an X host that is not a post (a profile, a search, a list),
    /// a `t.co` link or a mirror's link, sent to the URL check.
    XNotAPost = "x_not_a_post", 422, X;
    /// "We can't read this post on X. It may be deleted, protected or from a
    /// suspended account." (Kept on a refusal row for 24 hours.)
    XPostUnavailable = "x_post_unavailable", 422, X;
    /// The link names an earlier version of an edited post: nothing is
    /// scored or kept for it. Carries `url`, the canonical link of the
    /// post's current version ([`ApiRefusal::x_post_superseded`]), which the
    /// tool page posts once to the URL check. If that answer is superseded
    /// again: "This post was edited on X. Open it on X and check the link to
    /// its current version." A pending page drawn for the earlier link fails
    /// with this code.
    XPostSuperseded = "x_post_superseded", 422, X;
    /// X is rate-limiting our reads, or failed (a 5xx, a timeout): the reset
    /// time or a backoff is held until it passes.
    XBusy = "x_busy", 503, X;
    /// "Checking posts from X is unavailable right now. Paste the post's text
    /// to check it." (X capped our usage, has no credits for us or refused
    /// our token; the billing cycle's `X_READ_BUDGET` is reached; or X's
    /// answer was invalid.)
    XUnavailable = "x_unavailable", 503, X;
    /// The post's page was taken down at the request of its author or of X.
    /// A later check of that post answers it with no read; a removed X page
    /// carries it as its [`crate::page::Ended`] reason (the removal page,
    /// 451).
    XRemoved = "x_removed", 422, X;
    /// "This post is no longer available on X, so its report was removed."
    /// A deleted X page carries it as its [`crate::page::Ended`] reason.
    XGone = "x_gone", 410, X;
    /// "This post was edited on X after we checked it, so its report was
    /// removed. Check the post again from its link on X." A deleted X page
    /// carries it as its [`crate::page::Ended`] reason.
    XEdited = "x_edited", 410, X;
    /// "Reports on posts from X were removed." Every X page answers it once
    /// `x_purge` runs (`DETECT_X = purged`), as its
    /// [`crate::page::Ended`] reason.
    XPurged = "x_purged", 410, X;

    // ------------------------------------------------------------------- page
    /// "This secret link was deleted or has expired. Links are never reused."
    PageGone = "page_gone", 410, Page;
    /// "This page was removed after a legal notice."
    PageRemoved = "page_removed", 451, Page;
    /// "This page is unavailable while a report is reviewed."
    PageDisabled = "page_disabled", 451, Page;
    /// "This page belongs to another account; it was removed from your list
    /// only."
    NotOwner = "not_owner", 403, Page;
    /// The manage token does not match.
    ManageInvalid = "manage_invalid", 403, Page;
    /// "Saving is unavailable right now." (the account side failed).
    SaveUnavailable = "save_unavailable", 503, Page;

    // ------------------------------------------------------------------- paid
    /// Paid checks are off (`DETECT_PAID` or `BILLING`).
    PaidOff = "paid_off", 503, Paid;
    NotEnoughCredit = "not_enough_credit", 402, Paid;
    /// The quote seal is forged, expired or for another account or text.
    QuoteInvalid = "quote_invalid", 409, Paid;
    /// Another driver holds the job's lease.
    JobRunning = "job_running", 409, Paid;
    /// The route needs a session.
    SignedOut = "signed_out", 401, Paid;
    /// The paid API's key is invalid (never scored free).
    ApiKey = "api_key", 401, Paid;

    // ---------------------------------------------------------------- removal
    /// A required field of the track is missing.
    RemovalIncomplete = "removal_incomplete", 422, Removal;
    /// No case answers this status token.
    StatusUnknown = "status_unknown", 404, Removal;

    // --------------------------------------------------------------------- op
    OpMalformed = "op_malformed", 400, Op;
    OpBadSignature = "op_bad_signature", 403, Op;
    OpWrongTarget = "op_wrong_target", 403, Op;
    OpWrongEnv = "op_wrong_env", 403, Op;
    OpExpired = "op_expired", 403, Op;
    OpNotYetValid = "op_not_yet_valid", 403, Op;
    OpReusedNonce = "op_reused_nonce", 409, Op;
    OpUnknown = "op_unknown", 400, Op;
    /// `attach_email` at launch: no evidence kind is accepted.
    NoAcceptedEvidence = "no_accepted_evidence", 403, Op;

    // ----------------------------------------------------------------- signin
    /// The OAuth `state` is wrong, missing or reused.
    State = "state", 400, Signin;
    /// The flow, link or challenge expired.
    Expired = "expired", 410, Signin;
    /// The reader declined at the provider.
    Denied = "denied", 403, Signin;
    /// The provider failed or answered out of form.
    Provider = "provider", 502, Signin;
    /// "unavailable right now": the method is switched off.
    MethodOff = "method_off", 503, Signin;
    /// "An account already uses this address. Sign in with it, or send
    /// yourself a link to connect this method."
    EmailInUse = "email_in_use", 409, Signin;
    Throttled = "throttled", 429, Signin;
    /// "This sign-in needs an sr25519 (Substrate) account."
    WalletType = "wallet_type", 400, Signin;
    /// The wallet signature does not verify.
    Signature = "signature", 403, Signin;
    /// "Open this link in the browser where you asked to connect it, while
    /// signed in."
    LinkWrongBrowser = "link_wrong_browser", 403, Signin;
    /// Step-up with an identity that is not this account's.
    StepUpIdentity = "step_up_identity", 403, Signin;
    /// The flow needs a session and has none.
    Session = "session", 401, Signin;
    /// "This sign-in method can't be connected." (the identity's account is
    /// not active).
    LinkUnavailable = "link_unavailable", 403, Signin;
    /// The identity's account is locked or deleted: no session is issued.
    AccountUnavailable = "account_unavailable", 403, Signin;

    // ---------------------------------------------------------------- account
    /// The action needs a step-up inside the window.
    StepUpRequired = "step_up_required", 403, Account;
    /// The last sign-in method cannot be removed.
    LastMethod = "last_method", 409, Account;
    /// Both accounts hold credit or plans: the operator decides the merge.
    MergeNeedsOperator = "merge_needs_operator", 409, Account;
    /// `ACCOUNTS = read_only`: no new accounts, links or saves.
    AccountsReadOnly = "accounts_read_only", 503, Account;
    /// "Nothing is waiting to be connected."
    NothingPending = "nothing_pending", 409, Account;
    /// Account deletion without the typed word.
    ConfirmWord = "confirm_word", 400, Account;
}

impl RefusalCode {
    /// The codes `/signin/` prints from `?e=`.
    pub fn is_signin_error(self) -> bool {
        self.group() == Group::Signin
    }
}

impl Serialize for RefusalCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RefusalCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        RefusalCode::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown refusal code {s:?}")))
    }
}

impl std::fmt::Display for RefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An API refusal: `{"state":"refused","code":…}` plus the counts some codes
/// carry. Absent counts are omitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiRefusal {
    /// Always `"refused"`.
    pub state: RefusedTag,
    pub code: RefusalCode,
    /// The scored part's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    /// With `too_short`: the fewest words scored now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_words: Option<u32>,
    /// The whole text's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_words: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<CutKind>,
    /// With `x_post_link` and `x_post_superseded` only: the post's canonical
    /// URL (`https://x.com/i/status/<id>`, typed: nothing else parses), which
    /// the tool page posts to the URL check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<PostUrl>,
}

/// The codes that carry `url`.
pub const URL_CODES: &[RefusalCode] = &[RefusalCode::XPostLink, RefusalCode::XPostSuperseded];

/// A broken [`ApiRefusal`] form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefusalError(pub &'static str);

impl ApiRefusal {
    pub fn new(code: RefusalCode) -> ApiRefusal {
        ApiRefusal {
            state: RefusedTag,
            code,
            words: None,
            tokens: None,
            min_words: None,
            total_words: None,
            cut: None,
            url: None,
        }
    }

    /// `x_post_link` for post `id`; `None` for an id X never issues.
    pub fn x_post_link(id: u64) -> Option<ApiRefusal> {
        Some(ApiRefusal { url: Some(PostUrl::new(id)?), ..ApiRefusal::new(RefusalCode::XPostLink) })
    }

    /// `x_post_superseded`, naming `current`, the id of the post's current
    /// version; `None` for an id X never issues.
    pub fn x_post_superseded(current: u64) -> Option<ApiRefusal> {
        Some(ApiRefusal { url: Some(PostUrl::new(current)?), ..ApiRefusal::new(RefusalCode::XPostSuperseded) })
    }

    /// Checks the form: `url` is present exactly on the [`URL_CODES`] (its
    /// type already holds it to the canonical form).
    pub fn check(&self) -> Result<(), RefusalError> {
        match (self.url.is_some(), URL_CODES.contains(&self.code)) {
            (true, true) | (false, false) => Ok(()),
            (false, true) => Err(RefusalError("x_post_link and x_post_superseded carry their url")),
            (true, false) => Err(RefusalError("only x_post_link and x_post_superseded carry a url")),
        }
    }
}

/// The literal `"refused"`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefusedTag;

impl Serialize for RefusedTag {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("refused")
    }
}

impl<'de> Deserialize<'de> for RefusedTag {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s == "refused" { Ok(RefusedTag) } else { Err(serde::de::Error::custom("state must be \"refused\"")) }
    }
}

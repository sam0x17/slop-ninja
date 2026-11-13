//! Bodies of the detect Worker's forwarded routes that pages and the website
//! render from ([`crate::sn::RouteId`]). Refusals are [`crate::ApiRefusal`]
//! bodies with the status of their code.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::refusal::RefusalCode;
use crate::x::XPostUrl;

crate::code_enum! {
    /// Why a link was not saved to the account.
    pub enum SaveReason {
        /// The session was missing or expired: `save` became false.
        SignedOut = "signed_out",
        /// The account Worker failed; the link exists unsaved.
        AccountUnavailable = "account_unavailable",
    }
}

/// `POST /api/detect/v1/share` (Get a link).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRequest {
    /// The exact string that was scored.
    pub text: String,
    pub seal: String,
    pub expiry: Expiry,
    /// Save to my detections (needs a principal).
    pub save: bool,
    /// "Where is this text from?": only its host is kept, inside the
    /// encrypted document.
    pub source: Option<String>,
}

/// The new secret link. The browser opens `<secret origin>/<slug>#m=<manage>`
/// (with `&s=1` when saved).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareAnswer {
    pub slug: String,
    pub manage: String,
    pub saved: bool,
    pub reason: Option<SaveReason>,
}

/// `POST /api/detect/v1/rerun`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerunRequest {
    pub slug: String,
}

/// A re-run's new page: a secret copy carries its own manage token; a public
/// re-run answers the newest page's slug only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerunAnswer {
    pub slug: String,
    pub manage: Option<String>,
}

/// `POST /api/detect/v1/state` (a pending public page's poll).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRequest {
    pub slug: String,
}

/// A pending page's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateAnswer {
    pub state: DetState,
    /// The claim's lease ended: offer "Run it".
    pub lease_ended: bool,
    /// The failure, once failed.
    pub code: Option<RefusalCode>,
}

/// `POST /api/detect/v1/manage` on the secret host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManageRequest {
    pub slug: String,
    pub manage: String,
    pub action: ManageAction,
    /// With `set_expiry`.
    pub expiry: Option<Expiry>,
}

/// The page after a manage action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManageAnswer {
    pub ok: bool,
    pub state: DetState,
    pub expires_at: Option<u64>,
    /// A re-run copy's latest allowed expiry.
    pub latest_expiry: Option<u64>,
    /// The expiry choices this page offers (a copy offers only those that
    /// end no later than its original).
    #[serde(default)]
    pub choices: Vec<Expiry>,
}

/// `POST /api/detect/v1/paid/quote` (the website turns the form into this).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRequest {
    pub text: String,
    pub seal: String,
}

/// The quote page's figures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteAnswer {
    pub sections: u32,
    pub words: u32,
    /// Section 1, the free cut, which is not re-scored or billed.
    pub free_words: u32,
    /// Tokens past the free cut, at least the minimum.
    pub paid_tokens: u32,
    /// What it draws: plan first, then credit.
    pub draw_plan: u32,
    pub draw_credit: u32,
    /// The balance covers it: show **Confirm**.
    pub enough: bool,
    pub quote_seal: String,
}

/// `POST /api/detect/v1/paid/start`. The website answers 303 to
/// `<secret origin>/<slug>#m=<manage>&s=1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    pub text: String,
    pub seal: String,
    pub quote_seal: String,
    pub expiry: Expiry,
}

/// The paid job's page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartAnswer {
    pub slug: String,
    pub manage: String,
}

/// `POST /api/detect/v1/job` on the secret host: NDJSON lines of
/// [`JobLine`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRequest {
    pub slug: String,
}

/// One line of the job stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JobLine {
    /// "Checking section 3 of 5…".
    Running { section: u32, of: u32 },
    /// Another driver holds the lease: progress only.
    Progress { section: u32, of: u32 },
    /// Every section is done and the page is live: reload.
    Done,
    /// "We couldn't finish this check: ‹reason›. Nothing was charged."
    Failed { code: RefusalCode },
}

/// `POST /api/detect/v1/removal` (the website turns the form into this).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalRequest {
    pub step: RemovalStep,
    pub track: CaseKind,
    /// The page's link or slug.
    pub target: String,
    /// The track's fields by name (for copyright, the 512(c)(3) fields).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

/// `step = review`: the fields check out, or the missing ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalReviewAnswer {
    pub complete: bool,
    #[serde(default)]
    pub missing: Vec<String>,
}

crate::code_enum! {
    /// An automatic first step on intake.
    pub enum AutoStep {
        /// A complete copyright notice against a public page.
        ExcerptWithheld = "excerpt_withheld",
        /// An illegal-content report against a secret page.
        Disabled = "disabled",
        /// A wrong-text or wrong-result report.
        Recheck = "recheck",
    }
}

/// `step = send`: the case, shown once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalReceived {
    /// `td-` and 8 characters.
    pub case_id: String,
    /// 26 characters, for the status page's fragment.
    pub status_token: String,
    pub auto: Option<AutoStep>,
}

/// `POST /api/detect/v1/removal/status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalStatusRequest {
    pub token: String,
}

/// A case's state and dates; never the notice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalStatusAnswer {
    pub case_id: String,
    pub state: CaseState,
    pub received_at: u64,
    pub updated_at: u64,
    pub due_at: Option<u64>,
}

/// The body of the detect Worker's op route, `POST /api/detect/v1/op`:
/// `{"signed": <envelope>, "x_url": <link>}`, with `x_url` optional.
///
/// `signed` is the signed envelope as `slop-ninja-signed-op` defines it (its
/// `Wire`, verified as it stands by `Verifier::verify_wire`). `x_url` travels
/// beside it, outside the signed bytes: the post's link that `restore
/// {case_id}` needs on an X page, which the Worker accepts only when its
/// source key hashes to the tombstone's `canon_key`, and then drops. No other
/// op takes it; a Worker refuses `x_url` on any op but `restore`. Unknown
/// fields are refused. The account Worker's op route takes the bare envelope.
///
/// The type has no `Serialize`, because [`XPostUrl`] has none: a request can
/// be parsed, never written back out (the operator's CLI builds the JSON).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpRequest<W> {
    pub signed: W,
    #[serde(default)]
    pub x_url: Option<XPostUrl>,
}

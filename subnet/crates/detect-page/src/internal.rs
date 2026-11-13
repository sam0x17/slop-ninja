//! Request and answer bodies of the detect Worker's internal routes
//! ([`crate::sn::InternalRoute`]). Refusals are [`crate::ApiRefusal`] bodies.
//!
//! Slugs travel in request bodies, never in a URL. `owner_h` never crosses
//! a Worker boundary: the website passes the principal, and the detect
//! Worker derives the owner hash itself.

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::ids::IdH;
use crate::page::Page;

/// The most ids one `states` call carries: D1 binds at most 100 parameters
/// per statement, so 90 ids leave room for the statement's own.
pub const MAX_STATES_PER_CALL: usize = 90;

/// `POST /internal/v1/page`: the page JSON of a slug (website, secret host).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    /// A canonical slug: 7 characters (public) or 20 (secret).
    pub slug: String,
}

/// `POST /internal/v1/preview`: the unstored result of a sealed free check.
/// Writes nothing and does not consume the seal. A forged or expired seal,
/// or a text whose hash differs, answers `seal_invalid`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewRequest {
    pub text: String,
    pub seal: String,
}

/// `POST /internal/v1/card`: the card JSON of a slug ([`crate::Card`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRequest {
    pub slug: String,
}

/// `POST /internal/v1/states`: the states of saved detections, for the
/// account's list. At most [`MAX_STATES_PER_CALL`] ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatesRequest {
    pub id_h: Vec<IdH>,
}

/// The answer to [`StatesRequest`], one entry per id, in request order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatesAnswer {
    pub states: Vec<StateEntry>,
}

/// One saved detection's state, with expiry enforced on read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateEntry {
    pub id_h: IdH,
    /// `null`: no such detection (treated as deleted).
    pub state: Option<DetState>,
    pub expires_at: Option<u64>,
}

/// What the account list shows of a detection, fixed when it was saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub kind: Kind,
    pub band: Option<Band>,
    pub short: Option<ShortResult>,
    /// A paid document's strong and total sections.
    pub strong_sections: Option<u32>,
    pub total_sections: Option<u32>,
    pub words: u32,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

/// `POST /internal/v1/owner` with `X-SN-Principal`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerRequest {
    pub action: OwnerAction,
    pub slug: String,
    /// The manage token, for `peek` (to say whether it checks) and `claim`.
    pub manage: Option<String>,
}

/// `peek`: writes nothing. A deleted, expired, removed or disabled page
/// answers its refusal (`page_gone`, `page_removed`, `page_disabled`) with
/// no `id_h`, so nothing can be saved for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeekAnswer {
    pub id_h: IdH,
    pub summary: Summary,
    /// The manage token checks against the page.
    pub manage_ok: bool,
    pub owner: OwnerRelation,
}

/// `claim`: `is_owner` false means the save becomes a bookmark.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAnswer {
    pub id_h: IdH,
    pub summary: Summary,
    pub is_owner: bool,
    /// This claim set the owner (release it if the account save fails).
    pub newly_owned: bool,
}

/// `delete` and `release`: `{ok: true}`, or `{ok: false, code: "not_owner"}`
/// when the page is not this account's (nothing changes).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerDone {
    pub ok: bool,
    pub code: Option<crate::refusal::RefusalCode>,
}

/// `GET /internal/v1/examples`: the "What a report looks like" panel. Empty
/// until the operator sets examples; the panel is then not rendered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExamplesAnswer {
    #[serde(default)]
    pub examples: Vec<ExampleEntry>,
}

/// One example slot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExampleEntry {
    /// 1 to 4.
    pub slot: u8,
    pub source: ExampleSource,
    /// The report: `kind = example` for a synthetic slot, the public page for
    /// a public one.
    pub page: Page,
}

/// `POST /internal/v1/example`: one example's page JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExampleRequest {
    pub slot: u8,
}

/// `GET /internal/v1/removals`: the public removal record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalsAnswer {
    /// Public pages, newest first.
    #[serde(default)]
    pub entries: Vec<RemovalEntry>,
    /// Secret-page removals appear only as monthly counts.
    #[serde(default)]
    pub secret_monthly: Vec<MonthlyCount>,
}

/// One public removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalEntry {
    pub at: u64,
    pub public_slug: String,
    pub scope: RemovalScope,
    pub basis: String,
    pub jurisdiction: String,
}

/// Secret-page removals in one month.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthlyCount {
    /// `YYYY-MM`.
    pub month: String,
    pub count: u32,
}

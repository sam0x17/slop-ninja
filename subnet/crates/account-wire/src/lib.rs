//! Contracts of the SlopNinja account Worker.
//!
//! The account Worker binds no other Worker. The website calls it for the
//! session ([`SessionAnswer`]) and for every account page's view ([`View`]);
//! the website and the detect Worker call its saved-link routes
//! ([`OwnedAddRequest`] and the rest); the detect Worker calls its billing
//! routes ([`HoldRequest`] and the rest). The page scripts call its sign-in
//! APIs ([`EmailStartRequest`], [`WalletChallengeRequest`] and the rest), and
//! the account pages' forms post the bodies of [`form_fields`]
//! ([`LinkConfirmForm`] and the rest).
//!
//! Every timestamp is integer milliseconds UTC. User ids are
//! [`UserId`]s (32 hex digits); they appear only in internal bodies, never
//! in an answer a browser receives.
//!
//! The quota buckets of the `QuotaLimiter` Durable Object are fixed here
//! ([`Bucket`]): sign-in mail and connect mail draw on separate buckets, so a
//! connect request can never stop a sign-in mail.

use serde::{Deserialize, Serialize};
pub use slop_ninja_detect_page::{self as detect_page, BillingState, IdH, PurgeMode, UserId};
use slop_ninja_detect_page::{Channel, code_enum, internal::Summary};

/// The session cookie (host-only, `Secure; HttpOnly; SameSite=Lax; Path=/`).
pub const SESSION_COOKIE: &str = "__Host-sn_session";
/// The flow cookie: one OAuth, email-link or wallet flow, or one pending link.
pub const FLOW_COOKIE: &str = "__Host-sn_flow";
/// The policy the account Worker's limits come from (`auth-core::policy`).
pub const AUTH_POLICY: &str = "auth-policy/v1";
/// The policy the ledger's minimums come from.
pub const BILLING_POLICY: &str = "billing-policy/v1";

code_enum! {
    /// A sign-in method (`identities.provider`).
    pub enum Provider {
        Email = "email",
        Google = "google",
        Github = "github",
        X = "x",
        Bittensor = "bittensor",
    }
}

impl Provider {
    /// The kill switch of the method.
    pub fn switch(self) -> &'static str {
        match self {
            Provider::Email => "auth.email",
            Provider::Google => "auth.google",
            Provider::Github => "auth.github",
            Provider::X => "auth.x",
            Provider::Bittensor => "auth.bittensor",
        }
    }

    /// The OAuth providers (`/auth/<p>/start`).
    pub fn is_oauth(self) -> bool {
        matches!(self, Provider::Google | Provider::Github | Provider::X)
    }
}

code_enum! {
    /// A flow's intent (`auth_flows.intent`, `wallet_chals.intent`).
    pub enum Intent {
        Signin = "signin",
        Link = "link",
        StepUp = "step_up",
    }
}

code_enum! {
    /// An email link's intent (`email_tokens.intent`).
    pub enum MailIntent {
        Signin = "signin",
        Link = "link",
        StepUp = "step_up",
        /// Connect a provider whose verified address an account already has.
        AttachPending = "attach_pending",
        /// Later, only from an accepted `attach_email` op (recovery with a
        /// purchase receipt).
        Recover = "recover",
    }
}

code_enum! {
    /// How `/signin/` shows a method.
    pub enum MethodState {
        On = "on",
        /// Configured but switched off: "unavailable right now".
        Off = "off",
        /// Not configured: no button.
        Absent = "absent",
    }
}

code_enum! {
    /// A pending link's kind (`pending_links.kind`).
    pub enum PendingKind {
        Link = "link",
        Merge = "merge",
        EmailInUse = "email_in_use",
    }
}

code_enum! {
    /// An account's state (`users.state`).
    pub enum UserState {
        Active = "active",
        Locked = "locked",
        Deleted = "deleted",
    }
}

code_enum! {
    /// A source of credit (`payments.kind`). Grants are non-cash.
    pub enum PaymentKind {
        Grant = "grant",
        Processor = "processor",
        Tao = "tao",
    }
}

code_enum! {
    /// `payments.state`.
    pub enum PaymentState {
        Active = "active",
        Refunded = "refunded",
        Reversed = "reversed",
        Void = "void",
    }
}

code_enum! {
    /// `plan_periods.state`.
    pub enum PeriodState {
        Active = "active",
        Ended = "ended",
        Reversed = "reversed",
    }
}

code_enum! {
    /// `holds.state`.
    pub enum HoldState {
        Held = "held",
        Committed = "committed",
        Released = "released",
    }
}

code_enum! {
    /// How a hold ended (`hold_ends.kind`); a hold ends exactly once.
    pub enum HoldEnd {
        Committed = "committed",
        Released = "released",
    }
}

code_enum! {
    /// Where a hold's part draws from.
    pub enum PartSource {
        Period = "period",
        Payment = "payment",
    }
}

code_enum! {
    /// A stored avatar's type, from its magic bytes (`avatars.content_type`).
    pub enum AvatarType {
        Jpeg = "image/jpeg",
        Png = "image/png",
        Gif = "image/gif",
        Webp = "image/webp",
    }
}

code_enum! {
    /// The wallet sign-in mode.
    pub enum WalletMode {
        /// A browser extension signs the SIWS message.
        Extension = "extension",
        /// The reader signs a one-line message with `btcli` or `rtao`.
        Cli = "cli",
    }
}

// ------------------------------------------------------------------ session

/// `POST /internal/v1/session` (the cookie): the signed-in account, or 401.
/// When the session was renewed, the response also carries a `Set-Cookie`
/// header (the same token, a new `Max-Age`) for the website to pass on.
///
/// `user_id`, `auth_time` and `flags` are what the website forwards to the
/// detect Worker, as `X-SN-Principal` ([`SessionAnswer::principal`]).
/// `label`, `method` and `billing` are for the website's own rendering only
/// ("Checking as ‹label›" on the paid quote page, "Save to my detections ·
/// signed in as ‹label›" and the paid offer on the unstored result); the
/// website never forwards them to another Worker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAnswer {
    pub user_id: UserId,
    /// When the session last proved a sign-in method.
    pub auth_time: u64,
    /// The user's flags (`detect_page::sn::flags`).
    pub flags: u32,
    /// The identity the session signed in with: its address, `@handle` or
    /// SS58 address, as [`MeAnswer::label`] (shown to its holder only).
    pub label: String,
    /// That identity's method.
    pub method: Provider,
    /// The account Worker's `BILLING` state.
    pub billing: BillingState,
}

impl SessionAnswer {
    /// The `X-SN-Principal` value the website forwards: only `user_id`,
    /// `auth_time` and `flags`.
    pub fn principal(&self) -> detect_page::Principal {
        detect_page::Principal { user_id: self.user_id, auth_time_ms: self.auth_time, flags: self.flags }
    }

    /// The account is an operator's test account for paid checks
    /// (`set_flag {paid_preview}`).
    pub fn paid_preview(&self) -> bool {
        self.flags & detect_page::sn::flags::PAID_PREVIEW != 0
    }

    /// The account side of the unstored result's paid offer
    /// (`detect_page::page::Page::paid_offer`).
    pub fn paid_reader(&self) -> detect_page::page::PaidReader {
        detect_page::page::PaidReader { billing: self.billing, paid_preview: self.paid_preview() }
    }
}

/// `GET /api/account/v1/me` (the page scripts' one call): no ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeAnswer {
    pub signed_in: bool,
    /// "Signed in as ‹label›": the identity's address or handle.
    pub label: Option<String>,
    pub method: Option<Provider>,
    /// The account Worker's `BILLING` state, signed in or not (it is the same
    /// for every reader).
    pub billing: BillingState,
    /// The signed-in account is an operator's test account for paid checks;
    /// always `false` when signed out.
    pub paid_preview: bool,
}

impl MeAnswer {
    /// The account side of the unstored result's paid offer
    /// (`detect_page::page::Page::paid_offer`); `None` when signed out.
    pub fn paid_reader(&self) -> Option<detect_page::page::PaidReader> {
        self.signed_in
            .then_some(detect_page::page::PaidReader { billing: self.billing, paid_preview: self.paid_preview })
    }
}

// -------------------------------------------------------------------- views

code_enum! {
    /// The account pages a view is asked for.
    pub enum ViewPage {
        /// `/signin/` (switch states; step-up choices when signed in).
        Signin = "signin",
        Account = "account",
        LinkConfirm = "link_confirm",
        Sessions = "sessions",
        Detections = "detections",
        Billing = "billing",
        Delete = "delete",
    }
}

/// `POST /internal/v1/view` (the cookie): a page's view, or 401 when the
/// page needs a session and there is none (the website redirects to
/// `/signin/`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRequest {
    pub page: ViewPage,
    /// `/signin/?step_up=1`.
    #[serde(default)]
    pub step_up: bool,
}

/// One account page's view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "page", rename_all = "snake_case")]
pub enum View {
    Signin(SigninView),
    Account(AccountView),
    LinkConfirm(LinkConfirmView),
    Sessions(SessionsView),
    Detections(DetectionsView),
    Billing(BillingView),
    Delete(DeleteView),
}

/// A method and how `/signin/` or `/account/` shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodRow {
    pub method: Provider,
    pub state: MethodState,
}

/// `/signin/`: one list of equal buttons, then the email form. In step-up
/// mode only the account's own methods, with its own addresses to pick
/// from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigninView {
    pub signed_in: bool,
    /// In display order: Google, GitHub, X, Bittensor wallet, email.
    pub methods: Vec<MethodRow>,
    /// Step-up mode's choices (signed in, `step_up = true`).
    pub step_up: Option<Vec<StepUpChoice>>,
}

/// One of the account's own identities, offered for step-up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepUpChoice {
    pub identity_id: u64,
    pub provider: Provider,
    pub label: String,
    pub state: MethodState,
}

/// One sign-in method of the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRow {
    pub identity_id: u64,
    pub provider: Provider,
    /// The address, `@handle` or SS58 address (shown to its holder only).
    pub label: String,
    pub email_verified: bool,
    /// An X identity's avatar: `/api/account/v1/avatar/<ref>` (32 hex), or
    /// `null` for the neutral placeholder.
    pub avatar_ref: Option<String>,
    pub created_at: u64,
    pub last_used_at: u64,
    /// This session signed in with it.
    pub current: bool,
    /// Not the last method.
    pub removable: bool,
}

/// `/account/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountView {
    /// "Signed in as ‹label›".
    pub label: String,
    pub signed_in_with: Provider,
    pub identities: Vec<IdentityRow>,
    /// Methods that can be connected now.
    pub connect: Vec<MethodRow>,
    /// Inside the step-up window: connect, remove, export and delete need no
    /// step-up first.
    pub step_up_fresh: bool,
    /// `ACCOUNTS = read_only`.
    pub read_only: bool,
    pub billing: BillingState,
}

/// What is waiting to be connected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingView {
    /// `link` or `merge` (an `email_in_use` row is never shown here).
    pub kind: PendingKind,
    pub provider: Provider,
    /// The `@handle`, address or SS58 address to name.
    pub label: Option<String>,
    /// A merge refused while both accounts hold credit or plans.
    pub merge_blocked: bool,
}

/// `/account/link/confirm`. The form carries only the action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkConfirmView {
    pub label: String,
    /// `null`: "Nothing is waiting to be connected."
    pub pending: Option<PendingView>,
}

/// One session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    /// An opaque reference for **Sign out** (never the token or its hash).
    pub session_ref: String,
    pub created_at: u64,
    /// `YYYY-MM-DD`.
    pub last_seen_day: String,
    /// Browser and OS family.
    pub ua_family: Option<String>,
    pub method: Provider,
    pub current: bool,
}

/// `/account/sessions/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsView {
    pub label: String,
    pub sessions: Vec<SessionRow>,
}

/// One saved link, decrypted for its holder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedEntry {
    pub entry_id: u64,
    pub slug: String,
    pub id_h: IdH,
    /// This account owns the page (else a bookmark).
    pub is_owner: bool,
    pub summary: Summary,
}

/// `/account/detections/` (the website adds each entry's state from detect).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionsView {
    pub label: String,
    pub entries: Vec<OwnedEntry>,
    pub read_only: bool,
}

/// The active plan period.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanView {
    pub starts_at: u64,
    pub ends_at: u64,
    /// The period's tokens, and those used.
    pub tokens: u64,
    pub tokens_used: u64,
    /// The paid API's share of them, and the share used.
    pub api_tokens: u64,
    pub api_tokens_used: u64,
}

code_enum! {
    /// A billing history row.
    pub enum LedgerRowKind {
        /// Credit granted by the operator (non-cash).
        Grant = "grant",
        /// A plan period granted by the operator (non-cash).
        PlanGrant = "plan_grant",
        /// A payment (once payments open).
        Payment = "payment",
        /// A finished check's debit.
        Debit = "debit",
        /// A refund (once payments open).
        Refund = "refund",
        /// A voided grant.
        Void = "void",
    }
}

/// One billing history row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRow {
    pub at: u64,
    pub kind: LedgerRowKind,
    pub tokens: u64,
    /// Backed by cash (grants never are).
    pub cash: bool,
}

/// `/account/billing/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BillingView {
    pub label: String,
    pub state: BillingState,
    /// Credit left, in tokens.
    pub credit_tokens: u64,
    pub plan: Option<PlanView>,
    /// Newest first.
    pub history: Vec<LedgerRow>,
}

/// `/account/delete/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteView {
    pub label: String,
    /// Unused credit is lost on deletion.
    pub credit_tokens: u64,
    pub has_plan: bool,
    pub saved_pages: u32,
    pub owned_pages: u32,
    pub step_up_fresh: bool,
    /// The preselected choice for saved pages (delete).
    pub default_mode: PurgeMode,
}

// -------------------------------------------------------------- saved links

/// `POST /internal/v1/owned/add`. `id_h`, `summary` and `is_owner` only as
/// the detect Worker produced or answered them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedAddRequest {
    pub user_id: UserId,
    pub slug: String,
    pub id_h: IdH,
    pub summary: Summary,
    pub is_owner: bool,
}

/// The entry, new or existing (`UNIQUE (user_id, det_id_h)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedAddAnswer {
    pub entry_id: u64,
    pub added: bool,
}

/// `POST /internal/v1/owned/get` and `/owned/remove`: one entry of that user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedEntryRequest {
    pub user_id: UserId,
    pub entry_id: u64,
}

/// `POST /internal/v1/owned/prune`: entries found deleted or expired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPruneRequest {
    pub user_id: UserId,
    pub entry_ids: Vec<u64>,
}

/// How many rows a remove or prune deleted (only rows of that user).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedRemoved {
    pub removed: u32,
}

// ------------------------------------------------------------------ billing

/// `POST /internal/v1/billing/peek`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeekRequest {
    pub user_id: UserId,
}

/// What the account can draw, in tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeekAnswer {
    pub plan_left: u64,
    pub api_left: u64,
    pub credit_left: u64,
    pub paid_preview: bool,
}

/// `POST /internal/v1/billing/hold`: idempotent by `idem` (the quote nonce).
/// 402 `not_enough_credit` writes nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldRequest {
    pub user_id: UserId,
    pub tokens: u64,
    pub channel: Channel,
    pub idem: String,
}

/// One part of a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldPart {
    pub src: PartSource,
    pub id: u64,
    pub tokens: u64,
    /// Drawn from a cash-backed source.
    pub cash: bool,
}

/// A placed hold (new, or the existing one for this `idem`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldAnswer {
    pub hold_id: String,
    pub parts: Vec<HoldPart>,
}

/// `POST /internal/v1/billing/commit`: idempotent by `hold_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRequest {
    pub hold_id: String,
    pub receipt_id: String,
    pub package_pub: String,
}

/// `POST /internal/v1/billing/release`: idempotent by `hold_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRequest {
    pub hold_id: String,
}

/// How the hold ended: by this call, or earlier (a second commit, or a
/// release after a commit, answers the outcome already recorded).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldEndAnswer {
    pub hold_id: String,
    pub ended: HoldEnd,
    pub by_this_call: bool,
}

// -------------------------------------------------------------------- forms
//
// The bodies of the account pages' forms (`application/x-www-form-urlencoded`,
// posted by plain HTML forms, so every action works without JavaScript). The
// templates name each field as its type's `FIELDS` does; the Worker parses
// the body into the type and refuses any other field (`malformed_request`).
// No form carries a CSRF token (Fetch Metadata and `Origin` decide), and no
// form names an identity to attach: that comes from the server's own rows.

/// The word typed to confirm an account deletion.
pub const DELETE_CONFIRM_WORD: &str = "delete";

code_enum! {
    /// The action of the link-confirm form: the only thing it carries.
    pub enum LinkAction {
        /// Attach the identity of a pending row of kind `link`.
        Connect = "connect",
        /// Merge the other account into this one (a pending row of kind
        /// `merge` only). A merge runs nowhere else.
        Merge = "merge",
        /// Drop the pending row.
        Cancel = "cancel",
    }
}

/// `POST /account/link/confirm`: connect, merge or cancel what this
/// browser's pending row holds, under the session and step-up checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfirmForm {
    pub action: LinkAction,
}

impl LinkConfirmForm {
    pub const FIELDS: &'static [&'static str] = &["action"];
}

/// `POST /account/unlink`: remove one of the account's own sign-in methods
/// (step-up; refused for the last one with `last_method`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnlinkForm {
    /// An [`IdentityRow::identity_id`] of this account.
    pub identity_id: u64,
}

impl UnlinkForm {
    pub const FIELDS: &'static [&'static str] = &["identity_id"];
}

/// `POST /account/sessions/revoke`: sign one session out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRevokeForm {
    /// A [`SessionRow::session_ref`] of this account.
    pub session_ref: String,
}

impl SessionRevokeForm {
    pub const FIELDS: &'static [&'static str] = &["session_ref"];
}

/// `POST /account/delete` (step-up): the typed word and what happens to the
/// account's own pages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteAccountForm {
    /// The reader types [`DELETE_CONFIRM_WORD`]; anything else is refused
    /// with `confirm_word`.
    pub confirm: String,
    /// `delete` (preselected) or `release` (keep as anonymous secret links).
    pub mode: PurgeMode,
}

impl DeleteAccountForm {
    pub const FIELDS: &'static [&'static str] = &["confirm", "mode"];

    /// The typed word, ignoring surrounding spaces and ASCII case.
    pub fn confirmed(&self) -> bool {
        self.confirm.trim().eq_ignore_ascii_case(DELETE_CONFIRM_WORD)
    }
}

/// `POST /auth/logout`, `/account/sessions/revoke-all` and
/// `/account/export` (step-up): the form carries no field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyForm {}

impl EmptyForm {
    pub const FIELDS: &'static [&'static str] = &[];
}

/// `POST /account/detections/remove` and `/account/detections/delete`: one
/// saved link (the website's own routes, which call the detect and account
/// Workers in turn).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryForm {
    /// An [`OwnedEntry::entry_id`] of this account.
    pub entry_id: u64,
}

impl EntryForm {
    pub const FIELDS: &'static [&'static str] = &["entry_id"];
}

/// The fields of a forwarded account form route, or `None` for a route that
/// takes no form (the JSON APIs, the OAuth GETs, the op route and webhooks).
pub fn form_fields(route: detect_page::RouteId) -> Option<&'static [&'static str]> {
    use detect_page::RouteId as R;
    Some(match route {
        R::LinkConfirm => LinkConfirmForm::FIELDS,
        R::Unlink => UnlinkForm::FIELDS,
        R::SessionsRevoke => SessionRevokeForm::FIELDS,
        R::DeleteAccount => DeleteAccountForm::FIELDS,
        R::Logout | R::SessionsRevokeAll | R::Export => EmptyForm::FIELDS,
        _ => return None,
    })
}

// ---------------------------------------------------------- sign-in APIs

/// `POST /api/account/v1/email/start` (form or JSON). The answer never says
/// whether an account exists: always 303 to `/signin/check-email/` (form)
/// or 202 (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailStartRequest {
    /// Ignored for `step_up` (the identity names the address) and
    /// `attach_pending` (the pending row does).
    pub email: Option<String>,
    pub return_to: Option<String>,
    pub intent: Option<MailIntent>,
    /// `step_up`: one of the account's own email identities.
    pub identity_id: Option<u64>,
}

/// `POST /api/account/v1/email/peek` and `/email/confirm`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailTokenRequest {
    pub token: String,
}

/// Peek: read-only, does not spend the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailPeekAnswer {
    pub intent: MailIntent,
    /// The full address: whoever holds the token can sign in as it anyway.
    pub address: String,
    /// `attach_pending`: the provider waiting to be connected.
    pub provider: Option<Provider>,
}

/// Where a confirmed flow goes next: a path on the site's origin, which the
/// page checks is same-origin before it navigates. It is the success answer
/// of `/email/confirm` and of `/wallet/verify`, for every intent:
/// - `link`: always [`LINK_CONFIRM_PATH`]; only that page's POST attaches
///   the identity, and the pending row keeps the flow's own `return_to` for
///   afterwards;
/// - every other intent: the flow's stored `return_to` (from the start or
///   challenge request, after `auth-core::return_to::sanitize`), or, when it
///   was absent or refused, the intent's default ([`Intent::default_return_to`],
///   [`MailIntent::default_return_to`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextAnswer {
    pub return_to: String,
}

/// The page a `link` flow answers with ([`NextAnswer`]).
pub const LINK_CONFIRM_PATH: &str = "/account/link/confirm";
/// Where a flow started while signed in (`link`, `step_up`) goes when its
/// `return_to` was absent or refused.
pub const SIGNED_IN_RETURN: &str = "/account/";
/// Where a flow started while signed out (a sign-in, `attach_pending`,
/// `recover`) goes when its `return_to` was absent or refused.
pub const SIGNED_OUT_RETURN: &str = "/detect/";

impl Intent {
    /// The flow was started by a signed-in account (`link`, `step_up`).
    pub fn is_signed_in(self) -> bool {
        matches!(self, Intent::Link | Intent::StepUp)
    }

    /// The `return_to` a flow stores when the request's was absent or
    /// refused.
    pub fn default_return_to(self) -> &'static str {
        if self.is_signed_in() { SIGNED_IN_RETURN } else { SIGNED_OUT_RETURN }
    }

    /// The path a successful OAuth callback or `/wallet/verify` answers:
    /// [`LINK_CONFIRM_PATH`] for `link`, else the flow's stored `return_to`
    /// (already sanitized, or the default).
    pub fn next_path(self, stored_return_to: &str) -> &str {
        if self == Intent::Link { LINK_CONFIRM_PATH } else { stored_return_to }
    }
}

impl MailIntent {
    /// The mail was asked for by a signed-in account (`link`, `step_up`).
    pub fn is_signed_in(self) -> bool {
        matches!(self, MailIntent::Link | MailIntent::StepUp)
    }

    /// The `return_to` an email token stores when the request's was absent
    /// or refused.
    pub fn default_return_to(self) -> &'static str {
        if self.is_signed_in() { SIGNED_IN_RETURN } else { SIGNED_OUT_RETURN }
    }

    /// The path a successful `/email/confirm` answers: [`LINK_CONFIRM_PATH`]
    /// for `link`, else the token's stored `return_to`.
    pub fn next_path(self, stored_return_to: &str) -> &str {
        if self == MailIntent::Link { LINK_CONFIRM_PATH } else { stored_return_to }
    }
}

/// `POST /api/account/v1/wallet/challenge`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletChallengeRequest {
    /// SS58, any prefix.
    pub address: String,
    pub mode: WalletMode,
    pub intent: Intent,
    /// The page to land on after the flow (`/signin/?return_to=` or the
    /// account page's own path). The Worker stores only the value
    /// `auth-core::return_to::sanitize` re-serializes, in
    /// `wallet_chals.return_to`, or [`Intent::default_return_to`] when it is
    /// absent or refused; the raw value is never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_to: Option<String>,
}

/// The message to sign.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletChallengeAnswer {
    pub challenge_id: String,
    pub message: String,
}

/// `POST /api/account/v1/wallet/verify`. A success answers [`NextAnswer`]
/// for every intent ([`Intent::next_path`] of the challenge's intent and
/// stored `return_to`): `signin` with the new session's cookie, `step_up` with the
/// rotated session's, and `link` with [`LINK_CONFIRM_PATH`] after writing
/// the pending row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletVerifyRequest {
    pub challenge_id: String,
    /// 64 bytes, hex (with or without `0x`).
    pub signature: String,
}

// ------------------------------------------------------------------- quotas

code_enum! {
    /// A `QuotaLimiter` bucket kind. The Durable Object's key is
    /// `HMAC(K_quota, "<kind>:<key>")` ([`Bucket::name`]); the buckets are
    /// Durable Object state and need no D1 table.
    pub enum BucketKind {
        /// Per address (`addr_h`): sign-in mail whose link its holder can
        /// use (`signin`, `step_up`, `attach_pending`, `recover`). Policy
        /// value `email_ceiling`.
        Email = "email",
        /// Per address (`addr_h`): connect (`link`) mail. Policy value
        /// `email_link_ceiling`. When empty, connect requests to the address
        /// are refused; a sign-in mail is never stopped by it.
        EmailLink = "email_link",
        /// Per account (`user_id`): connect mail it requests. Policy value
        /// `link_mail_quota`.
        LinkMail = "link_mail",
        /// Per account (`user_id`): paid quotes. Policy value `quote_quota`.
        Quote = "quote",
        /// Per account (`user_id`, once payments open): checkouts. Policy value
        /// `checkout_quota`.
        Checkout = "checkout",
        /// Per API key prefix (the paid API, later). Policy value `api_quota`.
        Api = "api",
    }
}

impl BucketKind {
    /// The `auth-policy/v1` or `billing-policy/v1` value that sizes it.
    pub fn policy_name(self) -> &'static str {
        match self {
            BucketKind::Email => "email_ceiling",
            BucketKind::EmailLink => "email_link_ceiling",
            BucketKind::LinkMail => "link_mail_quota",
            BucketKind::Quote => "quote_quota",
            BucketKind::Checkout => "checkout_quota",
            BucketKind::Api => "api_quota",
        }
    }
}

/// One bucket: its kind and key (hex `addr_h`, hex `user_id` or a key
/// prefix).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Bucket {
    pub kind: BucketKind,
    pub key: String,
}

impl Bucket {
    /// `"<kind>:<key>"`, the HMAC input of the Durable Object's name.
    pub fn name(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.key)
    }
}

/// A `link` mail was asked for without the account that requested it. A
/// connect mail is only ever sent for a signed-in, stepped-up account, so
/// the caller refuses the send rather than skip that account's quota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoRequester;

/// The buckets a mail draws on: a `link` mail draws on the address's
/// `email_link` bucket and the requesting account's `link_mail` bucket,
/// never on the sign-in bucket, and needs `requester`; every other mail
/// draws on the address's `email` bucket only (`requester` is not used).
/// So every send that draws on the sign-in bucket is a link its holder can
/// use, and no connect mail escapes its account's quota.
pub fn mail_buckets(
    intent: MailIntent,
    addr_h_hex: &str,
    requester: Option<UserId>,
) -> Result<Vec<Bucket>, NoRequester> {
    match intent {
        MailIntent::Link => {
            let u = requester.ok_or(NoRequester)?;
            Ok(vec![
                Bucket { kind: BucketKind::EmailLink, key: addr_h_hex.to_owned() },
                Bucket { kind: BucketKind::LinkMail, key: u.hex() },
            ])
        }
        MailIntent::Signin | MailIntent::StepUp | MailIntent::AttachPending | MailIntent::Recover => {
            Ok(vec![Bucket { kind: BucketKind::Email, key: addr_h_hex.to_owned() }])
        }
    }
}

/// Why a `MAIL_SUBJECT_PREFIX` var was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadPrefix;

/// The longest subject prefix: room for a bracketed environment name.
pub const MAX_SUBJECT_PREFIX: usize = 24;

/// Checks the `MAIL_SUBJECT_PREFIX` var (`""` in production, `"[staging] "`
/// on staging): printable ASCII only, so it can never inject a header.
pub fn mail_subject_prefix(var: &str) -> Result<&str, BadPrefix> {
    if var.len() <= MAX_SUBJECT_PREFIX && var.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        Ok(var)
    } else {
        Err(BadPrefix)
    }
}

//! What the website renders the account pages from (`/internal/v1/view`),
//! the session answer (`/internal/v1/session`) and `/api/account/v1/me`.
//!
//! Every view is the holder's own data: addresses, handles and SS58
//! addresses appear only to the account that holds them. No view carries a
//! user id, a token or a token hash: sessions appear by an opaque keyed
//! reference, an X avatar by its random reference.

use slop_ninja_account_wire::{
    AccountView, BillingView, DeleteView, DetectionsView, IdentityRow, LedgerRow, LedgerRowKind, LinkConfirmView,
    MeAnswer, MethodRow, MethodState, PendingKind, PendingView, PlanView, Provider, SessionRow, SessionsView,
    SigninView, StepUpChoice, View, ViewPage, ViewRequest,
};
use slop_ninja_auth_core::providers::OAuthProvider;
use slop_ninja_detect_page::{PurgeMode, RefusalCode, UserId};

use crate::config::Config;
use crate::host::{DbError, Host};
use crate::http::{self, Req, Resp};
use crate::sessions::{self, Session};
use crate::store::{self, Switches};

/// Sign-in methods in display order.
pub const ORDER: [Provider; 5] =
    [Provider::Google, Provider::Github, Provider::X, Provider::Bittensor, Provider::Email];

/// How `/signin/` shows a method: absent when not configured (an OAuth
/// client without its id or secret, or mail without a sender), off when its
/// switch is off.
pub fn method_state(cfg: &Config, sw: &Switches, m: Provider) -> MethodState {
    let configured = match OAuthProvider::of(m) {
        Some(p) => cfg.client(p).is_some(),
        None => m != Provider::Email || cfg.mail.is_ok(),
    };
    if !configured {
        MethodState::Absent
    } else if sw.method_on(m) {
        MethodState::On
    } else {
        MethodState::Off
    }
}

fn methods(cfg: &Config, sw: &Switches) -> Vec<MethodRow> {
    ORDER.iter().map(|&m| MethodRow { method: m, state: method_state(cfg, sw, m) }).collect()
}

/// `POST /internal/v1/session`: the signed-in account, or 401. A renewed
/// session's refreshed cookie rides along for the website to pass on.
pub async fn session<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    match sessions::current(host, req).await {
        Ok(Some(s)) => {
            let r = Resp::json(200, &s.answer(sw.billing()));
            match sessions::renew_if_due(cfg, host, &s).await {
                Some(c) => r.with_cookie(c),
                None => r,
            }
        }
        Ok(None) => Resp::refusal(RefusalCode::Session),
        Err(_) => Resp::refusal(RefusalCode::Internal),
    }
}

/// `GET /api/account/v1/me`.
pub async fn me<H: Host>(host: &H, req: &Req, sw: &Switches) -> Resp {
    let s = sessions::current(host, req).await.ok().flatten();
    Resp::json(
        200,
        &MeAnswer {
            signed_in: s.is_some(),
            label: s.as_ref().map(|s| s.label.clone()),
            method: s.as_ref().map(|s| s.method),
            billing: sw.billing(),
            paid_preview: s.as_ref().is_some_and(|s| s.flags & slop_ninja_detect_page::sn::flags::PAID_PREVIEW != 0),
        },
    )
}

/// `POST /internal/v1/view`.
pub async fn view<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    let body: ViewRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    let s = match sessions::current(host, req).await {
        Ok(s) => s,
        Err(_) => return Resp::refusal(RefusalCode::Internal),
    };
    let built = match (body.page, &s) {
        (ViewPage::Signin, _) => signin(cfg, host, sw, s.as_ref(), body.step_up).await,
        (_, None) => return Resp::refusal(RefusalCode::Session),
        (ViewPage::Account, Some(s)) => account(cfg, host, sw, s).await,
        (ViewPage::LinkConfirm, Some(s)) => link_confirm(host, req, s).await,
        (ViewPage::Sessions, Some(s)) => sessions_view(cfg, host, s).await,
        (ViewPage::Detections, Some(s)) => crate::owned::entries(cfg, host, &s.user_id).await.map(|entries| {
            View::Detections(DetectionsView { label: s.label.clone(), entries, read_only: sw.read_only() })
        }),
        (ViewPage::Billing, Some(s)) => billing(host, sw, s).await,
        (ViewPage::Delete, Some(s)) => delete(cfg, host, s).await,
    };
    let Ok(v) = built else { return Resp::refusal(RefusalCode::Internal) };
    let r = Resp::json(200, &v);
    match &s {
        Some(s) => match sessions::renew_if_due(cfg, host, s).await {
            Some(c) => r.with_cookie(c),
            None => r,
        },
        None => r,
    }
}

async fn signin<H: Host>(
    cfg: &Config,
    host: &H,
    sw: &Switches,
    s: Option<&Session>,
    step_up: bool,
) -> Result<View, DbError> {
    let choices = match (s, step_up) {
        (Some(s), true) => {
            let rows = host
                .all(crate::stmt!(
                    "SELECT identity_id, provider, email, handle FROM identities WHERE user_id = ? ORDER BY identity_id",
                    s.user_id.0
                ))
                .await?;
            Some(
                rows.iter()
                    .filter_map(|r| {
                        let provider = Provider::parse(r.text("provider")?)?;
                        Some(StepUpChoice {
                            identity_id: r.u64("identity_id")?,
                            provider,
                            label: store::row_label(r),
                            state: method_state(cfg, sw, provider),
                        })
                    })
                    .collect(),
            )
        }
        _ => None,
    };
    Ok(View::Signin(SigninView { signed_in: s.is_some(), methods: methods(cfg, sw), step_up: choices }))
}

async fn account<H: Host>(cfg: &Config, host: &H, sw: &Switches, s: &Session) -> Result<View, DbError> {
    let rows = host
        .all(crate::stmt!(
            "SELECT i.identity_id, i.provider, i.email, i.email_verified, i.handle, i.created_at, i.last_used_at, \
             a.ref FROM identities i LEFT JOIN avatars a ON a.identity_id = i.identity_id WHERE i.user_id = ? \
             ORDER BY i.identity_id",
            s.user_id.0
        ))
        .await?;
    let removable = rows.len() > 1;
    let identities = rows
        .iter()
        .filter_map(|r| {
            let id = r.int("identity_id")?;
            Some(IdentityRow {
                identity_id: u64::try_from(id).ok()?,
                provider: Provider::parse(r.text("provider")?)?,
                label: store::row_label(r),
                email_verified: r.int("email_verified").unwrap_or(0) != 0,
                avatar_ref: r.blob("ref").map(hex::encode),
                created_at: r.u64("created_at")?,
                last_used_at: r.u64("last_used_at")?,
                current: id == s.identity_id,
                removable,
            })
        })
        .collect();
    let connect = methods(cfg, sw).into_iter().filter(|m| m.state != MethodState::Absent).collect();
    Ok(View::Account(AccountView {
        label: s.label.clone(),
        signed_in_with: s.method,
        identities,
        connect,
        step_up_fresh: s.stepped_up(cfg, host.now_ms()),
        read_only: sw.read_only(),
        billing: sw.billing(),
    }))
}

async fn link_confirm<H: Host>(host: &H, req: &Req, s: &Session) -> Result<View, DbError> {
    let pending = match crate::link::pending_row(host, req, s).await? {
        Some((_, p)) => {
            let kind = p.text("kind").and_then(PendingKind::parse);
            let provider = p.text("provider").and_then(Provider::parse);
            match (kind, provider) {
                (Some(kind), Some(provider)) => {
                    let merge_blocked = match (kind, p.b16("from_user_id")) {
                        (PendingKind::Merge, Some(b)) => store::both_hold_money(host, &s.user_id, &UserId(b)).await?,
                        _ => false,
                    };
                    Some(PendingView {
                        kind,
                        provider,
                        label: Some(store::label(provider, p.text("email"), p.text("handle"))),
                        merge_blocked,
                    })
                }
                _ => None,
            }
        }
        None => None,
    };
    Ok(View::LinkConfirm(LinkConfirmView { label: s.label.clone(), pending }))
}

async fn sessions_view<H: Host>(cfg: &Config, host: &H, s: &Session) -> Result<View, DbError> {
    let now = host.now_ms();
    let rows = host
        .all(crate::stmt!(
            "SELECT s.token_h, s.created_at, s.last_seen_day, s.ua_family, i.provider FROM sessions s \
             JOIN identities i ON i.identity_id = s.identity_id \
             WHERE s.user_id = ? AND s.idle_expires_at > ? AND s.abs_expires_at > ? ORDER BY s.created_at DESC",
            s.user_id.0,
            now,
            now
        ))
        .await?;
    let sessions = rows
        .iter()
        .filter_map(|r| {
            let token_h = r.blob("token_h")?;
            Some(SessionRow {
                session_ref: cfg.keys.session_ref(token_h),
                created_at: r.u64("created_at")?,
                last_seen_day: r.text("last_seen_day")?.to_owned(),
                ua_family: r.text("ua_family").map(str::to_owned),
                method: Provider::parse(r.text("provider")?)?,
                current: token_h == s.token_h.as_slice(),
            })
        })
        .collect();
    Ok(View::Sessions(SessionsView { label: s.label.clone(), sessions }))
}

/// Credit left: the active sources' tokens.
pub async fn credit<H: Host>(host: &H, user: &UserId) -> Result<u64, DbError> {
    let r = host
        .first(crate::stmt!(
            "SELECT COALESCE(SUM(tokens_left), 0) AS n FROM payments WHERE user_id = ? AND state = 'active'",
            user.0
        ))
        .await?;
    Ok(r.and_then(|r| r.u64("n")).unwrap_or(0))
}

async fn billing<H: Host>(host: &H, sw: &Switches, s: &Session) -> Result<View, DbError> {
    let now = host.now_ms();
    let plan = host
        .first(crate::stmt!(
            "SELECT starts_at, ends_at, allowance_granted, used, api_cap_granted, api_used FROM plan_periods \
             WHERE user_id = ? AND state = 'active' AND starts_at <= ? AND ends_at > ? ORDER BY starts_at DESC",
            s.user_id.0,
            now,
            now
        ))
        .await?
        .and_then(|r| {
            Some(PlanView {
                starts_at: r.u64("starts_at")?,
                ends_at: r.u64("ends_at")?,
                tokens: r.u64("allowance_granted")?,
                tokens_used: r.u64("used")?,
                api_tokens: r.u64("api_cap_granted")?,
                api_tokens_used: r.u64("api_used")?,
            })
        });
    let mut history = Vec::new();
    for r in host
        .all(crate::stmt!(
            "SELECT kind, cash, tokens_total, state, created_at FROM payments WHERE user_id = ? \
             AND NOT (tokens_total = 0 AND EXISTS (SELECT 1 FROM plan_periods p WHERE p.payment_id = payments.payment_id))",
            s.user_id.0
        ))
        .await?
    {
        let (Some(at), Some(tokens)) = (r.u64("created_at"), r.u64("tokens_total")) else { continue };
        let cash = r.int("cash").unwrap_or(0) != 0;
        let kind = match (r.text("kind"), r.text("state")) {
            (_, Some("void")) => LedgerRowKind::Void,
            (_, Some("refunded" | "reversed")) => LedgerRowKind::Refund,
            (Some("grant"), _) => LedgerRowKind::Grant,
            _ => LedgerRowKind::Payment,
        };
        history.push(LedgerRow { at, kind, tokens, cash });
    }
    for r in host
        .all(crate::stmt!("SELECT starts_at, allowance_granted, cash FROM plan_periods WHERE user_id = ?", s.user_id.0))
        .await?
    {
        let (Some(at), Some(tokens)) = (r.u64("starts_at"), r.u64("allowance_granted")) else { continue };
        let cash = r.int("cash").unwrap_or(0) != 0;
        history.push(LedgerRow {
            at,
            kind: if cash { LedgerRowKind::Payment } else { LedgerRowKind::PlanGrant },
            tokens,
            cash,
        });
    }
    for r in host
        .all(crate::stmt!(
            "SELECT at, tokens, cash FROM debits WHERE user_id = ? ORDER BY at DESC LIMIT 200",
            s.user_id.0
        ))
        .await?
    {
        let (Some(at), Some(tokens)) = (r.u64("at"), r.u64("tokens")) else { continue };
        history.push(LedgerRow { at, kind: LedgerRowKind::Debit, tokens, cash: r.int("cash").unwrap_or(0) != 0 });
    }
    history.sort_by_key(|r| std::cmp::Reverse(r.at));
    Ok(View::Billing(BillingView {
        label: s.label.clone(),
        state: sw.billing(),
        credit_tokens: credit(host, &s.user_id).await?,
        plan,
        history,
    }))
}

async fn delete<H: Host>(cfg: &Config, host: &H, s: &Session) -> Result<View, DbError> {
    let counts = host
        .first(crate::stmt!(
            "SELECT (SELECT COUNT(*) FROM owned WHERE user_id = ?) AS saved, \
             (SELECT COUNT(*) FROM owned WHERE user_id = ? AND is_owner = 1) AS owned, \
             (SELECT COUNT(*) FROM plan_periods WHERE user_id = ? AND state = 'active') AS plans",
            s.user_id.0,
            s.user_id.0,
            s.user_id.0
        ))
        .await?;
    let n = |c: &str| counts.as_ref().and_then(|r| r.u64(c)).unwrap_or(0);
    Ok(View::Delete(DeleteView {
        label: s.label.clone(),
        credit_tokens: credit(host, &s.user_id).await?,
        has_plan: n("plans") > 0,
        saved_pages: u32::try_from(n("saved")).unwrap_or(u32::MAX),
        owned_pages: u32::try_from(n("owned")).unwrap_or(u32::MAX),
        step_up_fresh: s.stepped_up(cfg, host.now_ms()),
        default_mode: PurgeMode::Delete,
    }))
}

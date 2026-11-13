//! Bittensor wallet sign-in: `wallet/challenge` and `wallet/verify`, in
//! extension mode (the SIWS layout) and command-line mode (one line).
//!
//! Nothing is counted per public key: every address is public on chain, so
//! a per-key quota would let anyone lock a wallet-only account out. A
//! challenge belongs to the browser that asked for it (its flow binding)
//! and is upserted on `(binding_h, pubkey)`, so a newer challenge from one
//! browser replaces its older one and no other browser's challenge is
//! touched. The verify takes the challenge with `DELETE … RETURNING` (a
//! reused id finds nothing), checks the binding and the expiry, then the
//! signature over the stored bytes, raw or `<Bytes>`-wrapped, and nothing
//! else (`rtao_core::keys::verify_strict` through auth-core).

use slop_ninja_account_wire::{
    Intent, LINK_CONFIRM_PATH, NextAnswer, Provider, WalletChallengeAnswer, WalletChallengeRequest, WalletVerifyRequest,
};
use slop_ninja_auth_core::return_to::{ReturnTo, sanitize_or_default};
use slop_ninja_auth_core::siws::{self, Challenge, ChallengeFacts, Nonce};
use slop_ninja_auth_core::state::Flow;
use slop_ninja_auth_core::token::Token;
use slop_ninja_auth_core::{cookie, csrf};
use slop_ninja_detect_page::{RefusalCode, UserId};

use crate::config::Config;
use crate::host::Host;
use crate::http::{self, Req, Resp};
use crate::sessions;
use crate::signin::{self, Done, FlowFacts, Proven};
use crate::store::{self, Switches};

const METHOD: &str = "bittensor";

fn json_body<T: serde::de::DeserializeOwned>(req: &Req) -> Result<T, RefusalCode> {
    if !csrf::content_type_ok(req.header("content-type"), csrf::BodyType::Json) {
        return Err(csrf::CONTENT_TYPE_REFUSAL);
    }
    http::json(&req.body)
}

/// `POST /api/account/v1/wallet/challenge`.
pub async fn challenge<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    match run_challenge(cfg, host, req, sw).await {
        Ok(r) => {
            crate::metrics::auth(host, METHOD, "start", None);
            r
        }
        Err(c) => {
            crate::metrics::auth(host, METHOD, "failed", Some(c));
            Resp::refusal(c)
        }
    }
}

async fn run_challenge<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Result<Resp, RefusalCode> {
    let body: WalletChallengeRequest = json_body(req)?;
    if !sw.method_on(Provider::Bittensor) {
        return Err(RefusalCode::MethodOff);
    }
    let pk = siws::decode_address(&body.address).map_err(|e| e.code())?;
    let now = host.now_ms();
    let (user_id, session_h) = if body.intent.is_signed_in() {
        let s = sessions::current(host, req).await.map_err(|_| RefusalCode::Internal)?.ok_or(RefusalCode::Session)?;
        if body.intent == Intent::Link {
            if sw.read_only() {
                return Err(RefusalCode::AccountsReadOnly);
            }
            if !s.stepped_up(cfg, now) {
                return Err(RefusalCode::StepUpRequired);
            }
        }
        (Some(s.user_id), Some(s.token_h))
    } else {
        (None, None)
    };
    let return_to =
        sanitize_or_default(body.return_to.as_deref(), &cfg.origins.site, &cfg.policy, body.intent.is_signed_in());
    let (binding, new_cookie) = match cookie::token(req.cookie(), cookie::FLOW) {
        Some(t) => (t, None),
        None => {
            let t = Token::from_random(host.random());
            let c = cookie::set(cookie::FLOW, &t, cfg.policy.flow_ttl_s.value);
            (t, Some(c))
        }
    };
    let nonce = Nonce::from_random(host.random());
    let expires = now + cfg.policy.wallet_challenge_ttl_s.value * 1000;
    let message = siws::message(
        body.mode,
        &Challenge {
            site: &cfg.origins.site,
            public_key: &pk,
            nonce: &nonce,
            issued_at_ms: now,
            expires_at_ms: expires,
        },
    );
    let id = Token::from_random(host.random());
    let stmts = vec![
        store::prune("wallet_chals", now, cfg.policy.flow_sweep.value.per_insert),
        crate::stmt!(
            "INSERT INTO wallet_chals (chal_h, binding_h, pubkey, message, intent, user_id, session_h, return_to, \
             created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (binding_h, pubkey) DO UPDATE SET chal_h = excluded.chal_h, message = excluded.message, \
             intent = excluded.intent, user_id = excluded.user_id, session_h = excluded.session_h, \
             return_to = excluded.return_to, created_at = excluded.created_at, expires_at = excluded.expires_at",
            id.hash(),
            binding.hash(),
            pk,
            message.as_bytes(),
            body.intent.as_str(),
            user_id.map(|u| u.0),
            session_h,
            return_to.as_str(),
            now,
            expires
        ),
    ];
    host.batch(stmts).await.map_err(|_| RefusalCode::Internal)?;
    let r = Resp::json(200, &WalletChallengeAnswer { challenge_id: id.encode(), message });
    Ok(match new_cookie {
        Some(c) => r.with_cookie(c),
        None => r,
    })
}

/// `POST /api/account/v1/wallet/verify`.
pub async fn verify<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Resp {
    match run_verify(cfg, host, req, sw).await {
        Ok(r) => {
            crate::metrics::auth(host, METHOD, "redeemed", None);
            r
        }
        Err(c) => {
            crate::metrics::auth(host, METHOD, "failed", Some(c));
            Resp::refusal(c)
        }
    }
}

async fn run_verify<H: Host>(cfg: &Config, host: &H, req: &Req, sw: &Switches) -> Result<Resp, RefusalCode> {
    let body: WalletVerifyRequest = json_body(req)?;
    if !sw.method_on(Provider::Bittensor) {
        return Err(RefusalCode::MethodOff);
    }
    let id = Token::parse(&body.challenge_id).ok_or(RefusalCode::Expired)?;
    let now = host.now_ms();
    let row = host
        .first(crate::stmt!(
            "DELETE FROM wallet_chals WHERE chal_h = ? RETURNING binding_h, pubkey, message, intent, user_id, \
             session_h, return_to, expires_at",
            id.hash()
        ))
        .await
        .map_err(|_| RefusalCode::Internal)?;
    let facts = row
        .as_ref()
        .and_then(|r| Some(ChallengeFacts { binding_h: r.b32("binding_h")?, expires_at_ms: r.u64("expires_at")? }));
    siws::check_challenge(facts.as_ref(), cookie::get(req.cookie(), cookie::FLOW), now)
        .map_err(|r| r.code(Flow::WalletChallenge))?;
    let row = row.ok_or(RefusalCode::Expired)?;
    let pk = row.b32("pubkey").ok_or(RefusalCode::Internal)?;
    let message = row.blob("message").ok_or(RefusalCode::Internal)?;
    let intent = row.text("intent").and_then(Intent::parse).ok_or(RefusalCode::Internal)?;
    let sig = siws::parse_signature(&body.signature).map_err(|e| e.code())?;
    siws::verify(message, &sig, &pk).map_err(|e| e.code())?;
    let return_to = ReturnTo::from_stored(
        row.text("return_to").unwrap_or(""),
        &cfg.origins.site,
        &cfg.policy,
        intent.is_signed_in(),
    );
    let flow = FlowFacts {
        intent,
        user_id: row.b16("user_id").map(UserId),
        session_h: row.b32("session_h"),
        return_to: return_to.as_str().to_owned(),
    };
    let proven = Proven {
        provider: Provider::Bittensor,
        subject: hex::encode(pk),
        email: None,
        email_verified: false,
        handle: Some(siws::address_42(&pk)),
        link_email: None,
    };
    let answer = |path: &str| Resp::json(200, &NextAnswer { return_to: path.to_owned() });
    Ok(match signin::apply(cfg, host, req, sw, &proven, &flow, RefusalCode::Session).await? {
        Done::Session { set_cookies, next, .. } => answer(intent.next_path(&next)).with_cookies(set_cookies),
        Done::Pending { set_cookie } => answer(LINK_CONFIRM_PATH).with_cookie(set_cookie),
        Done::AlreadyLinked => answer(LINK_CONFIRM_PATH),
        Done::EmailInUse { .. } => return Err(RefusalCode::Internal),
    })
}

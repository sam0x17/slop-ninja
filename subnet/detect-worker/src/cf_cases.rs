//! The removal and op routes on Cloudflare (wasm32 only). Every flow is a
//! pure function of `crate::db::Io` (`cases`, `ops`, tested natively on the
//! store's own migration); this module admits the request, reads its body
//! within its bound and answers.
//!
//! - `POST /api/detect/v1/removal`: the free check's admission (the `X-SN-*`
//!   allow-list, the CSRF check, `X-SN-Principal` from the site host only),
//!   a JSON body whose declared length is within `routes::REMOVAL_BODY_BYTES`
//!   and whose footprint is reserved from the isolate's scan memory before a
//!   byte is read.
//! - `POST /api/detect/v1/removal/status`: a small body, on either host.
//! - `POST /api/detect/v1/op`: the bare signed envelope, at most
//!   `ops::OP_BODY_MAX` bytes. The route takes no CSRF check (Δ5): the
//!   signature is the authority. `owner_match` reads the account Worker's
//!   ids through its binding (`/internal/v1/users`).
//!
//! Nothing here writes a console line or echoes input; every answer is
//! `no-store`.

use slop_ninja_account_wire::{UsersPage, UsersRequest};
use slop_ninja_detect_page::api::{RemovalRequest, RemovalStatusRequest};
use slop_ninja_detect_page::ids::UserId;
use slop_ninja_detect_page::{Origins, RefusalCode, RouteId};
use worker::worker_sys::web_sys;
use worker::{Env, Method, Request};

use crate::cases::{self, Accounts, RemovalAnswer};
use crate::cf::{Reserved, origins, waits};
use crate::cf_io::{self, now_ms};
use crate::cf_routes::{Secrets, declared, forwarded, json_body, no_room, ok, refused};
use crate::cf_store::{ACCOUNT, CfStore};
use crate::db::IoError;
use crate::ops::{self, OpCtx};
use crate::routes::{self, forwarded::Admission};

/// The largest answer read from the account Worker's `users` route.
const USERS_ANSWER_BYTES: usize = 64 * 1024;

/// The routes' store, keys and origins (no serving package is needed).
struct Ctx<'e> {
    io: CfStore<'e>,
    secrets: Secrets,
    origins: Origins,
}

fn ctx(env: &Env) -> Result<Ctx<'_>, RefusalCode> {
    let missing = RefusalCode::NotConfigured;
    Ok(Ctx {
        io: CfStore::new(env, waits(env).hold_ms).ok_or(missing)?,
        secrets: Secrets::of(env).ok_or(missing)?,
        origins: origins(env).ok_or(missing)?,
    })
}

/// `POST /api/detect/v1/removal`.
pub(crate) async fn removal(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::Removal;
    let principal = match forwarded(&req, env, route) {
        Ok(Admission::Forwarded { principal, .. }) => principal,
        Ok(Admission::Direct) => None,
        Err(c) => return refused(env, route, c),
    };
    let Some(n) = declared(&req) else { return refused(env, route, RefusalCode::LengthRequired) };
    if n > routes::REMOVAL_BODY_BYTES {
        return refused(env, route, RefusalCode::TooLarge);
    }
    let _held = match Reserved::take(routes::removal_footprint(n)) {
        Ok(h) => h,
        Err(e) => return refused(env, route, no_room(e)),
    };
    let bound = usize::try_from(n).unwrap_or(usize::MAX);
    let body = match json_body::<RemovalRequest>(&mut req, env, bound).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let c = match ctx(env) {
        Ok(c) => c,
        Err(code) => return refused(env, route, code),
    };
    match cases::removal(&c.io, c.secrets.keys(), &c.origins, principal, &body).await {
        Ok(RemovalAnswer::Review(a)) => ok(&a),
        Ok(RemovalAnswer::Received(a)) => ok(&a),
        Err(code) => refused(env, route, code),
    }
}

/// `POST /api/detect/v1/removal/status`.
pub(crate) async fn removal_status(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::RemovalStatus;
    if let Err(c) = forwarded(&req, env, route) {
        return refused(env, route, c);
    }
    let body = match json_body::<RemovalStatusRequest>(&mut req, env, routes::SMALL_BODY_BYTES).await {
        Ok(b) => b,
        Err(c) => return refused(env, route, c),
    };
    let Some(io) = CfStore::new(env, waits(env).hold_ms) else {
        return refused(env, route, RefusalCode::NotConfigured);
    };
    match cases::removal_status(&io, &body.token).await {
        Ok(a) => ok(&a),
        Err(code) => refused(env, route, code),
    }
}

/// `POST /api/detect/v1/op`.
pub(crate) async fn op(mut req: Request, env: &Env) -> web_sys::Response {
    let route = RouteId::DetectOp;
    if let Err(c) = forwarded(&req, env, route) {
        return refused(env, route, c);
    }
    let until = now_ms().saturating_add(waits(env).body_ms);
    let body = match cf_io::request_bytes_until(&mut req, ops::OP_BODY_MAX, Some(until)).await {
        Ok(b) => b,
        Err(e) => return refused(env, route, crate::cf::body_code(e)),
    };
    let c = match ctx(env) {
        Ok(c) => c,
        Err(code) => return refused(env, route, code),
    };
    let accounts = CfAccounts { env, wait_ms: waits(env).hold_ms };
    let octx = OpCtx { keys: c.secrets.keys(), origins: &c.origins, accounts: &accounts };
    match ops::op(&c.io, &octx, &body).await {
        Ok(v) => ok(&v),
        Err(code) => refused(env, route, code),
    }
}

/// The account Worker's `users` pages, through its service binding.
struct CfAccounts<'e> {
    env: &'e Env,
    wait_ms: u64,
}

impl Accounts for CfAccounts<'_> {
    async fn users_page(&self, after: Option<UserId>, limit: u32) -> Result<UsersPage, IoError> {
        let fetcher = self.env.service(ACCOUNT).map_err(|_| IoError)?;
        let text = serde_json::to_string(&UsersRequest { after, limit }).map_err(|_| IoError)?;
        let req =
            cf_io::internal_request("https://account/internal/v1/users", Method::Post, Some(&text), Some(self.wait_ms))
                .map_err(|_| IoError)?;
        // On the request itself: a request built from a web_sys request is
        // immutable through `headers_mut`, and the route refuses text/plain.
        req.inner().headers().set("Content-Type", "application/json").map_err(|_| IoError)?;
        let mut res = fetcher.fetch_request(req).await.map_err(|_| IoError)?;
        if res.status_code() != 200 {
            cf_io::discard(&mut res);
            return Err(IoError);
        }
        cf_io::response_json::<UsersPage>(&mut res, USERS_ANSWER_BYTES).await.map_err(|_| IoError)
    }
}

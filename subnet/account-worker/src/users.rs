//! `POST /internal/v1/users`: one page of account ids, for the detect
//! Worker's operator op `owner_match`.
//!
//! A strike is keyed by its page's owner hash, `HMAC(K_owner, user_id)`,
//! which only the detect Worker can derive and nothing inverts. To name the
//! account behind a strike, the detect Worker pages through every account id
//! here, derives each id's hash and compares. This route answers ids and
//! states only, in id order, and is binding-only like every internal route:
//! no forwarder reaches it. It reads nothing else of an account.

use slop_ninja_account_wire::{USERS_PAGE_MAX, UserEntry, UserState, UsersPage, UsersRequest};
use slop_ninja_detect_page::{RefusalCode, UserId};

use crate::host::Host;
use crate::http::{self, Req, Resp};

/// One page, after `after` (from the start without it).
pub async fn users<H: Host>(host: &H, req: &Req) -> Resp {
    let body: UsersRequest = match http::json(&req.body) {
        Ok(b) => b,
        Err(c) => return Resp::refusal(c),
    };
    if body.limit == 0 || body.limit > USERS_PAGE_MAX {
        return Resp::refusal(RefusalCode::MalformedRequest);
    }
    let limit = u64::from(body.limit);
    let stmt = match body.after {
        None => crate::stmt!("SELECT user_id, state FROM users ORDER BY user_id LIMIT ?", limit),
        Some(after) => {
            crate::stmt!("SELECT user_id, state FROM users WHERE user_id > ? ORDER BY user_id LIMIT ?", after.0, limit)
        }
    };
    let rows = match host.all(stmt).await {
        Ok(r) => r,
        Err(_) => return Resp::refusal(RefusalCode::Unavailable),
    };
    let users: Vec<UserEntry> = rows.iter().filter_map(entry).collect();
    let full = rows.len() == body.limit as usize;
    let next = if full { users.last().map(|e| e.user_id) } else { None };
    Resp::json(200, &UsersPage { users, next })
}

fn entry(r: &crate::host::Row) -> Option<UserEntry> {
    Some(UserEntry { user_id: UserId(r.b16("user_id")?), state: UserState::parse(r.text("state")?)? })
}

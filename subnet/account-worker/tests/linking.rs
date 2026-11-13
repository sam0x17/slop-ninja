//! Linking, step-up, merging, unlinking, saved links and deletion: the
//! login-CSRF and browser-binding cases, Δ4 (nothing connects to or merges
//! out of an account that is not active), the last-method refusal and the
//! owner contract's account side.

mod support;

use serde_json::{Value, json};
use support::*;

const A: &str = "reader-a@example.com";
const B_MAIL: &str = "reader-b@example.org";

fn body(r: &slop_ninja_account_worker::Resp) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

fn last_token(h: &TestHost, to: &str) -> String {
    h.mails_to(to).last().unwrap().token()
}

fn confirm(
    b: &mut Browser,
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    action: &str,
) -> slop_ninja_account_worker::Resp {
    b.post_form(h, cfg, "link_confirm", "/account/link/confirm", &[("action", action)])
}

#[test]
fn a_link_mail_works_only_in_the_session_that_asked() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    let mut c = Browser::new();
    c.email_sign_in(&h, &cfg, "reader-c@example.com");
    a.email_start(&h, &cfg, json!({"email": B_MAIL, "intent": "link"}));
    let m = h.mails_to(B_MAIL).last().unwrap().clone();
    assert_eq!(m.subject, "[dev] Connect this address to a SlopNinja account");
    // The address's holder opens it in their own browser: no button, no
    // session, and the token is spent.
    let mut victim = Browser::new();
    let peek = victim.post_json(&h, &cfg, "email_peek", "/api/account/v1/email/peek", json!({"token": m.token()}));
    assert_eq!((peek.status, body(&peek)["code"].clone()), (403, json!("link_wrong_browser")));
    let r = victim.email_confirm(&h, &cfg, &m.token());
    assert_eq!(body(&r)["code"], "link_wrong_browser");
    assert!(!victim.signed_in());
    assert_eq!(a.email_confirm(&h, &cfg, &m.token()).status, 410, "the token works once");
    // Another account's session cannot use a fresh one either.
    h.advance(61_000);
    a.email_start(&h, &cfg, json!({"email": B_MAIL, "intent": "link"}));
    let r = c.email_confirm(&h, &cfg, &last_token(&h, B_MAIL));
    assert_eq!(body(&r)["code"], "link_wrong_browser");
    // The asking session confirms: a pending row, then the explicit connect.
    h.advance(61_000);
    a.email_start(&h, &cfg, json!({"email": B_MAIL, "intent": "link"}));
    let r = a.email_confirm(&h, &cfg, &last_token(&h, B_MAIL));
    assert_eq!(body(&r)["return_to"], "/account/link/confirm");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities"), 2, "nothing attached before the POST");
    let v = a.view(&h, &cfg, "link_confirm");
    assert_eq!(v["pending"]["kind"], "link");
    assert_eq!(v["pending"]["label"], B_MAIL);
    let old = a.jar[SESSION].clone();
    let r = confirm(&mut a, &h, &cfg, "connect");
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/"));
    assert_ne!(a.jar[SESSION], old, "the session rotates at an identity change");
    assert!(!a.jar.contains_key(FLOW));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities"), 3);
    assert!(a.view(&h, &cfg, "link_confirm")["pending"].is_null());
}

#[test]
fn a_forged_connect_names_nothing_and_attaches_nothing() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    let r = a.post_form(
        &h,
        &cfg,
        "link_confirm",
        "/account/link/confirm",
        &[("action", "connect"), ("provider", "x"), ("subject", "1")],
    );
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/link/confirm?e=malformed_request"));
    let r = confirm(&mut a, &h, &cfg, "connect");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/link/confirm?e=nothing_pending"));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities"), 1);
}

#[test]
fn a_pending_row_belongs_to_its_browser_and_session() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    h.stub.borrow_mut().github.user = Some("bob".into());
    let r = a.oauth(&h, &cfg, "github", "?intent=link");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/link/confirm"));
    // C, signed in to another account, with A's flow cookie copied over.
    let mut c = Browser::new();
    c.email_sign_in(&h, &cfg, "reader-c@example.com");
    c.jar.insert(FLOW.into(), a.jar[FLOW].clone());
    let r = confirm(&mut c, &h, &cfg, "connect");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/link/confirm?e=nothing_pending"));
    assert!(c.view(&h, &cfg, "link_confirm")["pending"].is_null());
    // The right browser and session connect it.
    assert_eq!(a.view(&h, &cfg, "link_confirm")["pending"]["label"], "bob-example");
    assert_eq!(confirm(&mut a, &h, &cfg, "connect").status, 303);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'github'"), 1);
}

#[test]
fn link_and_step_up_flows_need_the_session_that_started_them() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    // An OAuth link callback arriving without the starting session.
    let start = a.get(&h, &cfg, "auth_start", "/auth/google/start?intent=link");
    let back = h.authorize_location(start.header("location").unwrap());
    let u = url::Url::parse(&back).unwrap();
    let mut thief = Browser::new();
    thief.jar.insert(FLOW.into(), a.jar[FLOW].clone());
    let r = thief.get(&h, &cfg, "auth_callback", &format!("{}?{}", u.path(), u.query().unwrap()));
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=session"));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM pending_links"), 0);
    // Starting a link with no session writes nothing.
    let r = Browser::new().get(&h, &cfg, "auth_start", "/auth/google/start?intent=link");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=session"));
    // Outside the step-up window a link start goes to step-up first.
    h.advance(11 * 60_000);
    let r = a.get(&h, &cfg, "auth_start", "/auth/google/start?intent=link");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?step_up=1&return_to=%2Faccount%2F"));
    // A wallet link challenge verified in another user's session.
    h.advance(1);
    let mut a2 = Browser::new();
    a2.email_sign_in(&h, &cfg, A);
    let kp = rtao_core::keys::Keypair::from_uri("//SlopNinjaAccountTestTwo", rtao_core::keys::CRYPTO_SR25519).unwrap();
    let c = a2.post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": kp.ss58_address(), "mode": "cli", "intent": "link"}),
    );
    let c = body(&c);
    let mut other = Browser::new();
    other.email_sign_in(&h, &cfg, "reader-c@example.com");
    other.jar.insert(FLOW.into(), a2.jar[FLOW].clone());
    let sig = hex::encode(kp.sign(c["message"].as_str().unwrap().as_bytes()).unwrap());
    let r = other.post_json(
        &h,
        &cfg,
        "wallet_verify",
        "/api/account/v1/wallet/verify",
        json!({"challenge_id": c["challenge_id"], "signature": sig}),
    );
    assert_eq!(body(&r)["code"], "session");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM pending_links"), 0);
}

#[test]
fn step_up_needs_one_of_the_accounts_own_identities() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    let before = a.session(&h, &cfg).unwrap()["auth_time"].clone();
    // Google's alice is not an identity of this account.
    h.advance(60_000);
    let r = a.oauth(&h, &cfg, "google", "?intent=step_up");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=step_up_identity"));
    assert_eq!(a.session(&h, &cfg).unwrap()["auth_time"], before);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities"), 1);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users"), 1, "no account for the foreign identity");
    // The account's own address: the token rotates and auth_time moves.
    let id = a.view(&h, &cfg, "account")["identities"][0]["identity_id"].as_u64().unwrap();
    a.email_start(&h, &cfg, json!({"intent": "step_up", "identity_id": id, "email": "ignored@example.org"}));
    assert!(h.mails_to("ignored@example.org").is_empty(), "a typed address is ignored");
    let m = h.mails_to(A).last().unwrap().clone();
    assert_eq!(m.subject, "[dev] Confirm it's you on SlopNinja");
    let old = a.jar[SESSION].clone();
    h.advance(1_000);
    let r = a.email_confirm(&h, &cfg, &m.token());
    assert_eq!(r.status, 200);
    assert_ne!(a.jar[SESSION], old);
    assert!(a.session(&h, &cfg).unwrap()["auth_time"].as_u64().unwrap() > before.as_u64().unwrap());
    let v = a.view(&h, &cfg, "signin");
    assert!(v["step_up"].is_null(), "step-up choices only when asked");
    let v: Value = body(&a.internal(&h, &cfg, "/internal/v1/view", json!({"page": "signin", "step_up": true})));
    assert_eq!(v["step_up"][0]["provider"], "email");
}

fn save(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    user: &str,
    slug: &str,
    det: u8,
    owner: bool,
) -> slop_ninja_account_worker::Resp {
    let mut website = Browser::new();
    website.internal(
        h,
        cfg,
        "/internal/v1/owned/add",
        json!({
            "user_id": user, "slug": slug, "id_h": hex::encode([det; 32]), "is_owner": owner,
            "summary": {"kind": "private_text", "band": null, "short": null, "strong_sections": null,
                        "total_sections": null, "words": 300, "created_at": 1, "expires_at": null}
        }),
    )
}

#[test]
fn saved_links_keep_only_secret_slugs_and_only_their_users_rows() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    let ua = user_of(&mut a, &h, &cfg);
    let r = save(&h, &cfg, &ua, "abc1234", 1, false);
    assert_eq!(r.status, 400, "a public slug is never saved");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM owned"), 0);
    let slug = "0123456789abcdefghjk";
    let r = save(&h, &cfg, &ua, slug, 2, false);
    assert_eq!(body(&r)["added"], true);
    let r = save(&h, &cfg, &ua, slug, 2, true);
    assert_eq!(body(&r)["added"], false);
    let v = a.view(&h, &cfg, "detections");
    assert_eq!(v["entries"][0]["slug"], slug);
    assert_eq!(v["entries"][0]["is_owner"], true);
    assert!(!h.dump().contains(slug), "the store holds no slug in clear");
    let entry = v["entries"][0]["entry_id"].clone();
    let mut website = Browser::new();
    let other = "00000000000000000000000000000001";
    let r = website.internal(&h, &cfg, "/internal/v1/owned/get", json!({"user_id": other, "entry_id": entry}));
    assert_eq!(r.status, 404);
    let r = website.internal(&h, &cfg, "/internal/v1/owned/remove", json!({"user_id": other, "entry_id": entry}));
    assert_eq!(body(&r)["removed"], 0);
    let r = website.internal(&h, &cfg, "/internal/v1/owned/prune", json!({"user_id": ua, "entry_ids": [entry]}));
    assert_eq!(body(&r)["removed"], 1);
}

fn two_accounts(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
) -> (Browser, Browser, String, String) {
    let mut a = Browser::new();
    a.email_sign_in(h, cfg, A);
    h.stub.borrow_mut().github.user = Some("bob".into());
    let mut b = Browser::new();
    b.oauth(h, cfg, "github", "");
    let (ua, ub) = (user_of(&mut a, h, cfg), user_of(&mut b, h, cfg));
    (a, b, ua, ub)
}

#[test]
fn a_merge_moves_the_other_account_and_can_be_undone() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let (mut a, mut b, ua, ub) = two_accounts(&h, &cfg);
    save(&h, &cfg, &ub, "0123456789abcdefghjk", 7, true);
    save(&h, &cfg, &ub, "1123456789abcdefghjk", 8, false);
    save(&h, &cfg, &ua, "2123456789abcdefghjk", 8, false);
    let r = a.oauth(&h, &cfg, "github", "?intent=link");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/link/confirm"));
    let v = a.view(&h, &cfg, "link_confirm");
    assert_eq!(v["pending"]["kind"], "merge");
    assert_eq!(v["pending"]["merge_blocked"], false);
    // Connect is not merge: the action must be the row's.
    assert!(confirm(&mut a, &h, &cfg, "connect").header("location").unwrap().ends_with("e=nothing_pending"));
    let r = confirm(&mut a, &h, &cfg, "merge");
    assert_eq!(r.status, 303, "{:?}", r.header("location"));
    assert!(b.session(&h, &cfg).is_none(), "the merged account's sessions are revoked");
    let users = h.q("SELECT state, merged_into FROM users WHERE state = 'deleted'");
    assert_eq!(users.len(), 1);
    assert_eq!(hex::encode(users[0].blob("merged_into").unwrap()), ua);
    let v = a.view(&h, &cfg, "detections");
    let slugs: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e["slug"].as_str().unwrap()).collect();
    assert_eq!(slugs.len(), 2, "{slugs:?}");
    assert!(slugs.contains(&"0123456789abcdefghjk"));
    let q = h.queue.borrow();
    let rekey = q.iter().find(|m| matches!(m.job, slop_ninja_detect_page::queue::Job::OwnerRekey { .. })).unwrap();
    assert_eq!(
        serde_json::to_value(rekey).unwrap(),
        json!({"v": 1, "type": "owner_rekey", "from_user_id": ub, "to_user_id": ua})
    );
    drop(q);
    // The operator undoes it from the journal row.
    let seq = h.q("SELECT seq FROM journal WHERE op LIKE '{\"args\":%\"op\":\"merge\"%'")[0].int("seq").unwrap();
    let (status, ans) = op(&h, &cfg, "undo_merge", json!({"seq": seq, "reason": "holder asked within the window"}));
    assert_eq!(status, 200, "{ans}");
    assert_eq!(ans["detect_owner_unchanged"], true);
    let mut b2 = Browser::new();
    b2.oauth(&h, &cfg, "github", "");
    assert_eq!(user_of(&mut b2, &h, &cfg), ub);
    let v = b2.view(&h, &cfg, "detections");
    assert_eq!(v["entries"].as_array().unwrap().len(), 1, "B's own saved link went back and opens under B");
}

#[test]
fn a_merge_is_refused_while_both_accounts_hold_tokens() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let (mut a, _b, ua, ub) = two_accounts(&h, &cfg);
    for u in [&ua, &ub] {
        h.db.borrow()
            .execute(
                "INSERT INTO payments (user_id, kind, cash, tokens_total, tokens_left, state, created_at, journal_seq) \
                 VALUES (?, 'grant', ?, ?, ?, 'active', ?, ?)",
                rusqlite::params![hex::decode(u).unwrap(), 0, 900, 900, 1, 0],
            )
            .unwrap();
    }
    a.oauth(&h, &cfg, "github", "?intent=link");
    assert_eq!(a.view(&h, &cfg, "link_confirm")["pending"]["merge_blocked"], true);
    let r = confirm(&mut a, &h, &cfg, "merge");
    assert!(r.header("location").unwrap().ends_with("e=merge_needs_operator"));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users WHERE state = 'deleted'"), 0);
}

#[test]
fn nothing_connects_to_or_merges_out_of_an_account_that_is_not_active() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let (mut a, _b, _ua, ub) = two_accounts(&h, &cfg);
    // A lock landing between the pending row and the confirm.
    a.oauth(&h, &cfg, "github", "?intent=link");
    assert_eq!(a.view(&h, &cfg, "link_confirm")["pending"]["kind"], "merge");
    let (status, _) = op(&h, &cfg, "disable_account", json!({"user_id": ub, "reason": "abuse report"}));
    assert_eq!(status, 200);
    let r = confirm(&mut a, &h, &cfg, "merge");
    assert!(r.header("location").unwrap().ends_with("e=link_unavailable"), "{:?}", r.header("location"));
    assert_eq!(
        h.count(
            "SELECT COUNT(*) AS n FROM identities WHERE user_id = (SELECT user_id FROM users WHERE state = 'locked')"
        ),
        1
    );
    // A new link flow naming the locked account's identity.
    h.advance(1);
    let r = a.oauth(&h, &cfg, "github", "?intent=link");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=link_unavailable"));
    // Its own sign-in gets no session, and its address no email_in_use row.
    let mut locked = Browser::new();
    let r = locked.oauth(&h, &cfg, "github", "");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=account_unavailable"));
    assert!(!locked.signed_in());
    let (status, _) = op(&h, &cfg, "restore_account", json!({"user_id": ub, "reason": "cleared"}));
    assert_eq!(status, 200);
    assert!(Browser::new().oauth(&h, &cfg, "github", "").header("location").unwrap().ends_with("/detect/"));
}

#[test]
fn the_last_method_stays_and_unlinking_x_drops_its_avatar_and_sessions() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.oauth(&h, &cfg, "x", "");
    let x_id = a.view(&h, &cfg, "account")["identities"][0]["identity_id"].as_u64().unwrap().to_string();
    let r = a.post_form(&h, &cfg, "unlink", "/account/unlink", &[("identity_id", &x_id)]);
    assert!(r.header("location").unwrap().ends_with("e=last_method"));
    // Connect an address, from a second browser that signs in with it.
    a.email_start(&h, &cfg, json!({"email": A, "intent": "link"}));
    a.email_confirm(&h, &cfg, &last_token(&h, A));
    assert_eq!(confirm(&mut a, &h, &cfg, "connect").status, 303);
    let mut mail_browser = Browser::new();
    mail_browser.email_sign_in(&h, &cfg, A);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM avatars"), 1);
    // Unlinking X (from the email session) drops the avatar and X's sessions.
    let mut x2 = Browser::new();
    x2.oauth(&h, &cfg, "x", "");
    let r = mail_browser.post_form(&h, &cfg, "unlink", "/account/unlink", &[("identity_id", &x_id)]);
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/"));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM avatars"), 0);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'x'"), 0);
    assert!(x2.session(&h, &cfg).is_none());
    assert!(a.session(&h, &cfg).is_none(), "the X-signed session is revoked too");
    assert!(mail_browser.session(&h, &cfg).is_some());
    // Outside the step-up window, unlink goes to step-up first.
    h.advance(11 * 60_000);
    let id = mail_browser.view(&h, &cfg, "account")["identities"][0]["identity_id"].as_u64().unwrap().to_string();
    let r = mail_browser.post_form(&h, &cfg, "unlink", "/account/unlink", &[("identity_id", &id)]);
    assert!(r.header("location").unwrap().contains("/signin/?step_up=1"));
}

#[test]
fn deleting_an_account_purges_its_pages_first_and_keeps_nothing_personal() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, A);
    let ua = user_of(&mut a, &h, &cfg);
    save(&h, &cfg, &ua, "0123456789abcdefghjk", 9, true);
    let r = a.post_form(&h, &cfg, "delete_account", "/account/delete", &[("confirm", "remove"), ("mode", "release")]);
    assert!(r.header("location").unwrap().ends_with("e=confirm_word"));
    // The queue is down: nothing changes.
    h.queue_down.set(true);
    let r = a.post_form(&h, &cfg, "delete_account", "/account/delete", &[("confirm", "Delete"), ("mode", "release")]);
    assert!(r.header("location").unwrap().ends_with("e=internal"));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities"), 1);
    h.queue_down.set(false);
    let r = a.post_form(&h, &cfg, "delete_account", "/account/delete", &[("confirm", " Delete "), ("mode", "release")]);
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/"));
    assert!(!a.signed_in());
    let q = h.queue.borrow();
    assert_eq!(
        serde_json::to_value(&q[0]).unwrap(),
        json!({"v": 1, "type": "owner_purge", "user_id": ua, "mode": "release"})
    );
    drop(q);
    for t in ["identities", "sessions", "owned", "avatars"] {
        assert_eq!(h.count(&format!("SELECT COUNT(*) AS n FROM {t}")), 0, "{t}");
    }
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users WHERE state = 'deleted' AND flags = 0"), 1);
    let dump = h.dump();
    assert!(dump.lines().filter(|l| l.starts_with("journal.")).all(|l| !l.contains("reader-a")));
    // Signing in again makes a new account.
    let mut again = Browser::new();
    again.email_sign_in(&h, &cfg, A);
    assert_ne!(user_of(&mut again, &h, &cfg), ua);
}

#[test]
fn read_only_accounts_still_sign_in_but_open_nothing_new() {
    let h = TestHost::new();
    let open = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &open, A);
    let cfg = config(&[("ACCOUNTS", "read_only")]);
    let mut again = Browser::new();
    again.email_sign_in(&h, &cfg, A);
    assert!(again.signed_in());
    let mut fresh = Browser::new();
    fresh.email_start(&h, &cfg, json!({"email": "new@example.org"}));
    let r = fresh.email_confirm(&h, &cfg, &last_token(&h, "new@example.org"));
    assert_eq!(body(&r)["code"], "accounts_read_only");
    let ua = user_of(&mut a, &h, &cfg);
    assert_eq!(save(&h, &cfg, &ua, "0123456789abcdefghjk", 1, false).status, 503);
}

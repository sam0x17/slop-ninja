//! Sign-in end to end against the account store's own migration and the
//! dev auth stub: email links (and no lockout by address), Google, GitHub
//! and X (with X's slow exchange, a denial, a missing and an unverified
//! address), the X avatar, the wallet in both modes, sessions and cookies.

mod support;

use serde_json::json;
use slop_ninja_account_worker::host::Host;
use support::*;

const ALICE: &str = "alice@example.com";

#[test]
fn an_email_link_signs_in_and_the_cookies_are_host_only() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut b = Browser::new();
    let start = b.email_start(&h, &cfg, json!({"email": "  Alice@Example.COM ", "return_to": "/d/abc1234"}));
    assert_eq!(start.status, 202);
    let flow = start.set_cookies().into_iter().find(|c| c.starts_with(FLOW)).unwrap().to_owned();
    assert!(flow.ends_with("; Path=/; Secure; HttpOnly; SameSite=Lax"), "{flow}");
    assert!(!flow.contains("Domain"));
    let mails = h.mails_to(ALICE);
    assert_eq!(mails.len(), 1);
    // The recipient is the identity's subject, byte for byte.
    assert_eq!(mails[0].to, ALICE);
    assert_eq!(mails[0].subject, "[dev] Your SlopNinja sign-in link");
    assert!(mails[0].text.contains("https://slop.localhost:8787/auth/email/confirm#t="));
    // Peek is read-only and shows the address as typed.
    let peek = b.post_json(&h, &cfg, "email_peek", "/api/account/v1/email/peek", json!({"token": mails[0].token()}));
    assert_eq!(peek.status, 200);
    let peek: serde_json::Value = serde_json::from_slice(&peek.body).unwrap();
    assert_eq!(peek["intent"], "signin");
    assert_eq!(peek["address"], "Alice@Example.COM");
    let r = b.email_confirm(&h, &cfg, &mails[0].token());
    assert_eq!(r.status, 200);
    let next: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(next["return_to"], "/d/abc1234");
    let session = r.set_cookies().into_iter().find(|c| c.starts_with(SESSION)).unwrap().to_owned();
    assert!(session.contains("Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Lax"), "{session}");
    assert!(!b.jar.contains_key(FLOW), "the flow cookie is cleared at sign-in");
    let me = b.me(&h, &cfg);
    assert_eq!(me["signed_in"], true);
    assert_eq!(me["method"], "email");
    // The token works once.
    let again = Browser::new().email_confirm(&h, &cfg, &mails[0].token());
    assert_eq!(again.status, 410);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users"), 1);
    assert_eq!(
        h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'email' AND subject = 'alice@example.com'"),
        1
    );
}

#[test]
fn a_bypass_return_to_lands_on_the_origin() {
    let h = TestHost::new();
    let cfg = config(&[]);
    for bad in ["//evil.example/", "/\\evil.example", "https://evil.example/", "/%2f%2fevil.example", "/elsewhere/"] {
        let mut b = Browser::new();
        h.advance(61_000);
        assert_eq!(b.email_start(&h, &cfg, json!({"email": ALICE, "return_to": bad})).status, 202);
        let m = h.mails_to(ALICE).last().unwrap().clone();
        let r = b.email_confirm(&h, &cfg, &m.token());
        let next: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(next["return_to"], "/detect/", "{bad}");
    }
}

#[test]
fn nobody_else_can_void_delay_or_spend_the_holders_sign_in_link() {
    let h = TestHost::new();
    let cfg = config(&[]);
    // Another account, signed in and stepped up.
    let mut other = Browser::new();
    other.email_sign_in(&h, &cfg, "other@example.org");
    let mut a = Browser::new();
    assert_eq!(a.email_start(&h, &cfg, json!({"email": ALICE})).status, 202);
    let a_token = h.mails_to(ALICE)[0].token();
    // Browser B (no flow cookie each time) asks five links for the address,
    // and the other account asks to connect it three times.
    for _ in 0..5 {
        assert_eq!(Browser::new().email_start(&h, &cfg, json!({"email": ALICE})).status, 202);
    }
    for _ in 0..3 {
        h.advance(61_000);
        assert_eq!(other.email_start(&h, &cfg, json!({"email": ALICE, "intent": "link"})).status, 202);
    }
    // A's own link still confirms.
    let r = a.email_confirm(&h, &cfg, &a_token);
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    // The other account spent the address's connect budget: a fourth
    // connect request sends nothing, and raised the connect ceiling alert.
    let before = h.mails_to(ALICE).len();
    h.advance(61_000);
    other.email_start(&h, &cfg, json!({"email": ALICE, "intent": "link"}));
    assert_eq!(h.mails_to(ALICE).len(), before);
    assert_eq!(h.alerts("auth_link_ceiling"), 1);
    // The holder's next sign-in mail is still sent and confirms (Δ3).
    let mut a2 = Browser::new();
    a2.email_sign_in(&h, &cfg, ALICE);
    assert!(a2.signed_in());
}

#[test]
fn one_browser_gets_one_link_a_minute_and_ten_a_day() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut b = Browser::new();
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    // A double click: the second request inside the spacing sends nothing.
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.mails_to(ALICE).len(), 1);
    h.advance(60_000);
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.mails_to(ALICE).len(), 2);
    // Up to ten in a day; the tenth raises the ceiling alert, the eleventh
    // is not sent and answers the same.
    for _ in 0..8 {
        h.advance(61_000);
        assert_eq!(b.email_start(&h, &cfg, json!({"email": ALICE})).status, 202);
    }
    assert_eq!(h.mails_to(ALICE).len(), 10);
    assert_eq!(h.alerts("auth_mail_ceiling"), 1);
    h.advance(61_000);
    assert_eq!(b.email_start(&h, &cfg, json!({"email": ALICE})).status, 202);
    assert_eq!(h.mails_to(ALICE).len(), 10);
    // One refill every 2.4 hours.
    h.advance(8_640_000);
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.mails_to(ALICE).len(), 11);
    // Or at once after the operator's reset, journaled without the address.
    h.advance(61_000);
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.mails_to(ALICE).len(), 11);
    let (status, ans) =
        op(&h, &cfg, "reset_quota", json!({"kind": "email", "key": ALICE, "reason": "support request"}));
    assert_eq!(status, 200, "{ans}");
    h.advance(61_000);
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.mails_to(ALICE).len(), 12);
    let journal = h.q("SELECT op, reason FROM journal");
    assert!(journal.iter().all(|r| !r.text("op").unwrap().contains("alice")), "{journal:?}");
}

#[test]
fn malformed_addresses_send_nothing_and_answer_the_same() {
    let h = TestHost::new();
    let cfg = config(&[]);
    for bad in ["k\u{212a}ate@example.com", "a@b@example.com", "\"q\"@example.com", "x@[127.0.0.1]", "nobody"] {
        let r = Browser::new().email_start(&h, &cfg, json!({"email": bad}));
        assert_eq!(r.status, 202, "{bad}");
    }
    assert!(h.mail.borrow().is_empty());
    // The form answer is the same 303 for any address.
    let r = Browser::new().post_form(&h, &cfg, "email_start", "/api/account/v1/email/start", &[("email", "nobody")]);
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/check-email/"));
}

#[test]
fn a_suppressed_address_is_not_mailed_until_the_operator_lifts_it() {
    let h = TestHost::new();
    let cfg = config(&[]);
    *h.fail_mail.borrow_mut() = Some(slop_ninja_auth_core::mail::SendFailure::Suppressed);
    let mut b = Browser::new();
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM email_suppress"), 1);
    h.advance(61_000);
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert!(h.mails_to(ALICE).is_empty());
    let (status, ans) = op(&h, &cfg, "unsuppress", json!({"address": ALICE, "reason": "holder asked"}));
    assert_eq!(status, 200, "{ans}");
    assert_eq!(ans["removed"], true);
    h.advance(61_000);
    b.email_start(&h, &cfg, json!({"email": ALICE}));
    assert_eq!(h.mails_to(ALICE).len(), 1);
    // Neither the suppression list nor the journal holds the address.
    let rows = h.q("SELECT op, reason FROM journal");
    assert!(rows.iter().all(|r| !r.text("op").unwrap().contains("alice")), "{rows:?}");
    let dump = h.dump();
    assert!(
        dump.lines()
            .filter(|l| l.starts_with("email_suppress.") || l.starts_with("journal."))
            .all(|l| !l.contains("alice"))
    );
}

#[test]
fn google_and_github_sign_in_and_create_one_account_each() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut g = Browser::new();
    let r = g.oauth(&h, &cfg, "google", "?return_to=%2Faccount%2F");
    assert_eq!(r.status, 303, "{:?}", r.header("location"));
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/account/"));
    assert_eq!(g.me(&h, &cfg)["label"], "alice@example.com");
    let mut gh = Browser::new();
    h.stub.borrow_mut().github.user = Some("bob".into());
    let r = gh.oauth(&h, &cfg, "github", "");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/detect/"));
    assert_eq!(gh.me(&h, &cfg)["label"], "bob-example");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users"), 2);
    // A replayed callback finds no flow.
    let mut c = Browser::new();
    let start = c.get(&h, &cfg, "auth_start", "/auth/google/start");
    let back = h.authorize_location(start.header("location").unwrap());
    let u = url::Url::parse(&back).unwrap();
    let pq = format!("{}?{}", u.path(), u.query().unwrap());
    assert_eq!(c.get(&h, &cfg, "auth_callback", &pq).status, 303);
    let replay = c.get(&h, &cfg, "auth_callback", &pq);
    assert_eq!(replay.header("location"), Some("https://slop.localhost:8787/signin/?e=state"));
    // A callback in another browser (no flow cookie) is refused.
    let start = Browser::new().get(&h, &cfg, "auth_start", "/auth/github/start");
    let back = h.authorize_location(start.header("location").unwrap());
    let u = url::Url::parse(&back).unwrap();
    let r = Browser::new().get(&h, &cfg, "auth_callback", &format!("{}?{}", u.path(), u.query().unwrap()));
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=state"));
}

#[test]
fn a_provider_address_already_in_use_never_links_silently() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut owner = Browser::new();
    owner.email_sign_in(&h, &cfg, ALICE);
    // Google says alice's verified address: no sign-in, no new account.
    let mut g = Browser::new();
    let r = g.oauth(&h, &cfg, "google", "");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=email_in_use"));
    assert!(!g.signed_in());
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users"), 1);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM pending_links WHERE kind = 'email_in_use'"), 1);
    // The attach mail goes to the address, bound to this browser.
    assert_eq!(g.email_start(&h, &cfg, json!({"intent": "attach_pending"})).status, 202);
    let m = h.mails_to(ALICE).last().unwrap().clone();
    assert_eq!(m.subject, "[dev] Connect your Google sign-in to SlopNinja");
    // Another browser cannot use it.
    let peek =
        Browser::new().post_json(&h, &cfg, "email_peek", "/api/account/v1/email/peek", json!({"token": m.token()}));
    assert_eq!(peek.status, 403);
    let r = Browser::new().email_confirm(&h, &cfg, &m.token());
    assert_eq!(r.status, 403);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'google'"), 0);
    // A new link, confirmed in the browser that ran the Google sign-in.
    h.advance(61_000);
    g.email_start(&h, &cfg, json!({"intent": "attach_pending"}));
    let m = h.mails_to(ALICE).last().unwrap().clone();
    let r = g.email_confirm(&h, &cfg, &m.token());
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert!(g.signed_in());
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'google'"), 1);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users"), 1);
    assert_eq!(user_of(&mut g, &h, &cfg), user_of(&mut owner, &h, &cfg));
}

#[test]
fn x_signs_in_with_its_avatar_and_x_failures_issue_nothing() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut x = Browser::new();
    let r = x.oauth(&h, &cfg, "x", "");
    assert_eq!(r.status, 303, "{:?}", r.header("location"));
    assert!(x.signed_in());
    let v = x.view(&h, &cfg, "account");
    let id = &v["identities"][0];
    assert_eq!(id["label"], "@alice_example");
    let reference = id["avatar_ref"].as_str().unwrap().to_owned();
    let a = x.get(&h, &cfg, "avatar", &format!("/api/account/v1/avatar/{reference}"));
    assert_eq!(a.status, 200);
    assert_eq!(a.header("content-type"), Some("image/png"));
    assert_eq!(a.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(a.header("content-security-policy"), Some("default-src 'none'; sandbox"));
    assert_eq!(a.header("cache-control"), Some("private, max-age=3600"));
    // Another account's session gets the same 404 as an unknown reference.
    let mut other = Browser::new();
    other.email_sign_in(&h, &cfg, "other@example.org");
    let theirs = other.get(&h, &cfg, "avatar", &format!("/api/account/v1/avatar/{reference}"));
    let unknown = other.get(&h, &cfg, "avatar", &format!("/api/account/v1/avatar/{}", "0".repeat(32)));
    assert_eq!((theirs.status, &theirs.body), (unknown.status, &unknown.body));
    assert_eq!(theirs.status, 404);
    // A new sign-in draws a new reference; the old one answers 404.
    let mut x2 = Browser::new();
    x2.oauth(&h, &cfg, "x", "");
    let new_ref = x2.view(&h, &cfg, "account")["identities"][0]["avatar_ref"].as_str().unwrap().to_owned();
    assert_ne!(new_ref, reference);
    assert_eq!(x2.get(&h, &cfg, "avatar", &format!("/api/account/v1/avatar/{reference}")).status, 404);
    // X's code lives 30 seconds: a slow exchange issues nothing.
    h.stub.borrow_mut().x.token_delay_ms = 31_000;
    let mut slow = Browser::new();
    let r = slow.oauth(&h, &cfg, "x", "");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=provider"));
    assert!(!slow.signed_in());
    h.stub.borrow_mut().x.token_delay_ms = 0;
    // A denial at the consent screen.
    h.stub.borrow_mut().x.deny = true;
    let r = Browser::new().oauth(&h, &cfg, "x", "");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=denied"));
    h.stub.borrow_mut().x.deny = false;
    // X refusing for its limits (402): "unavailable right now", and an alert.
    h.stub.borrow_mut().x.userinfo_status = Some(402);
    let r = Browser::new().oauth(&h, &cfg, "x", "");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=method_off"));
    assert_eq!(h.alerts("x_refused"), 1);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users"), 2);
}

#[test]
fn x_avatars_that_are_not_images_from_its_host_store_nothing() {
    for (who, avatar) in
        [("carol", None), ("dave", None), ("erin", None), ("alice", Some("redirect")), ("bob", Some("missing"))]
    {
        let h = TestHost::new();
        let cfg = config(&[]);
        let mut s = h.stub.borrow_mut();
        s.x.user = Some(who.into());
        if let Some(a) = avatar {
            s.x.avatar = Some(serde_json::from_value(json!(a)).unwrap());
        }
        drop(s);
        let mut b = Browser::new();
        let r = b.oauth(&h, &cfg, "x", "");
        assert_eq!(r.status, 303, "{who}");
        assert!(b.signed_in(), "{who}: a missing or unverified address is allowed for X");
        assert_eq!(h.count("SELECT COUNT(*) AS n FROM avatars"), 0, "{who} {avatar:?}");
        assert!(b.view(&h, &cfg, "account")["identities"][0]["avatar_ref"].is_null());
    }
}

#[test]
fn x_sign_in_without_a_client_id_is_absent() {
    let h = TestHost::new();
    let cfg = config(&[("X_CLIENT_ID", "")]);
    let mut b = Browser::new();
    let v = b.view(&h, &cfg, "signin");
    let x = v["methods"].as_array().unwrap().iter().find(|m| m["method"] == "x").unwrap();
    assert_eq!(x["state"], "absent");
    let r = b.get(&h, &cfg, "auth_start", "/auth/x/start");
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/?e=method_off"));
    // Configured but switched off: "unavailable right now".
    let cfg = config(&[("auth.x", "off")]);
    let v = b.view(&h, &cfg, "signin");
    let x = v["methods"].as_array().unwrap().iter().find(|m| m["method"] == "x").unwrap();
    assert_eq!(x["state"], "off");
}

#[test]
fn an_unverified_google_address_is_not_an_email_and_never_links() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut owner = Browser::new();
    owner.email_sign_in(&h, &cfg, "carol@example.org");
    h.stub.borrow_mut().google.user = Some("carol".into());
    let mut g = Browser::new();
    let r = g.oauth(&h, &cfg, "google", "");
    assert_eq!(r.status, 303);
    assert!(g.signed_in(), "an unverified address cannot claim the email account");
    assert_ne!(user_of(&mut g, &h, &cfg), user_of(&mut owner, &h, &cfg));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'google' AND email_verified = 0"), 1);
}

fn sign(kp: &rtao_core::keys::Keypair, bytes: &[u8]) -> String {
    hex::encode(kp.sign(bytes).unwrap())
}

fn keypair(uri: &str) -> rtao_core::keys::Keypair {
    rtao_core::keys::Keypair::from_uri(uri, rtao_core::keys::CRYPTO_SR25519).unwrap()
}

#[test]
fn a_wallet_signs_in_raw_or_wrapped_and_nobody_can_lock_it_out() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let kp = keypair("//SlopNinjaAccountTestOne");
    let address = kp.ss58_address();
    // Other clients ask a thousand challenges for this key.
    for _ in 0..1000 {
        let r = Browser::new().post_json(
            &h,
            &cfg,
            "wallet_challenge",
            "/api/account/v1/wallet/challenge",
            json!({"address": address, "mode": "extension", "intent": "signin"}),
        );
        assert_eq!(r.status, 200);
    }
    let mut b = Browser::new();
    let first = b.post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": address, "mode": "extension", "intent": "signin"}),
    );
    let first: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    // A second challenge from this browser replaces its first.
    let c = b.post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": address, "mode": "extension", "intent": "signin", "return_to": "/account/"}),
    );
    let c: serde_json::Value = serde_json::from_slice(&c.body).unwrap();
    let message = c["message"].as_str().unwrap();
    assert!(message.starts_with("slop.localhost:8787 wants you to sign in with your Bittensor account:\n"));
    let wrapped = [b"<Bytes>".as_slice(), message.as_bytes(), b"</Bytes>"].concat();
    let stale = b.post_json(&h, &cfg, "wallet_verify", "/api/account/v1/wallet/verify", json!({"challenge_id": first["challenge_id"], "signature": sign(&kp, first["message"].as_str().unwrap().as_bytes())}));
    assert_eq!(stale.status, 410);
    // A signature over other bytes fails and spends the challenge.
    let c2 = b.post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": address, "mode": "extension", "intent": "signin", "return_to": "/account/"}),
    );
    let c2: serde_json::Value = serde_json::from_slice(&c2.body).unwrap();
    let bad = b.post_json(
        &h,
        &cfg,
        "wallet_verify",
        "/api/account/v1/wallet/verify",
        json!({"challenge_id": c2["challenge_id"], "signature": sign(&kp, b"other bytes")}),
    );
    assert_eq!(bad.status, 403);
    assert!(!b.signed_in());
    let _ = c;
    let c3 = b.post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": address, "mode": "extension", "intent": "signin", "return_to": "/account/"}),
    );
    let c3: serde_json::Value = serde_json::from_slice(&c3.body).unwrap();
    let wrapped3 = [b"<Bytes>".as_slice(), c3["message"].as_str().unwrap().as_bytes(), b"</Bytes>"].concat();
    let ok = b.post_json(
        &h,
        &cfg,
        "wallet_verify",
        "/api/account/v1/wallet/verify",
        json!({"challenge_id": c3["challenge_id"], "signature": sign(&kp, &wrapped3)}),
    );
    assert_eq!(ok.status, 200, "{}", String::from_utf8_lossy(&ok.body));
    let next: serde_json::Value = serde_json::from_slice(&ok.body).unwrap();
    assert_eq!(next["return_to"], "/account/");
    assert!(b.signed_in());
    assert_eq!(b.me(&h, &cfg)["label"], address);
    let _ = wrapped;
    // The command-line mode, signed raw, signs in to the same account.
    let mut cli = Browser::new();
    let c = cli.post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": address, "mode": "cli", "intent": "signin"}),
    );
    let c: serde_json::Value = serde_json::from_slice(&c.body).unwrap();
    assert!(c["message"].as_str().unwrap().starts_with("slop.localhost:8787 sign-in for "));
    let ok = cli.post_json(&h, &cfg, "wallet_verify", "/api/account/v1/wallet/verify", json!({"challenge_id": c["challenge_id"], "signature": format!("0x{}", sign(&kp, c["message"].as_str().unwrap().as_bytes()))}));
    assert_eq!(ok.status, 200);
    assert_eq!(user_of(&mut cli, &h, &cfg), user_of(&mut b, &h, &cfg));
    // An Ethereum-style address is not an sr25519 one.
    let r = Browser::new().post_json(
        &h,
        &cfg,
        "wallet_challenge",
        "/api/account/v1/wallet/challenge",
        json!({"address": "0x0000000000000000000000000000000000000000", "mode": "extension", "intent": "signin"}),
    );
    assert_eq!(r.status, 400);
}

#[test]
fn sessions_renew_daily_and_sign_out_everywhere() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut one = Browser::new();
    one.email_sign_in(&h, &cfg, ALICE);
    let mut two = Browser::new();
    two.email_sign_in(&h, &cfg, ALICE);
    // The same day: no renewal.
    assert!(one.internal(&h, &cfg, "/internal/v1/session", json!({})).set_cookies().is_empty());
    h.advance(86_400_000);
    let r = one.internal(&h, &cfg, "/internal/v1/session", json!({}));
    assert_eq!(r.status, 200);
    let before = one.jar[SESSION].clone();
    let c = r.set_cookies();
    assert_eq!(c.len(), 1, "renewed once");
    assert!(c[0].starts_with(&format!("{SESSION}={before};")), "renewal keeps the token");
    let v = two.view(&h, &cfg, "sessions");
    assert_eq!(v["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(v["sessions"][0]["ua_family"], "Firefox on Linux");
    let r = one.post_form(&h, &cfg, "sessions_revoke_all", "/account/sessions/revoke-all", &[]);
    assert_eq!(r.status, 303);
    assert!(!one.signed_in());
    assert!(two.session(&h, &cfg).is_none(), "the second browser is signed out");
    // Idle sessions end, and the hourly sweep deletes them.
    let mut three = Browser::new();
    three.email_sign_in(&h, &cfg, ALICE);
    h.advance(31 * 86_400_000);
    assert!(three.session(&h, &cfg).is_none());
    assert_eq!(slop_ninja_account_worker::host::Host::now_ms(&h), h.now.get());
    let swept = block_on(slop_ninja_account_worker::sweep::run(&h, "0 * * * *"));
    assert!(swept >= 1);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM sessions"), 0);
}

#[test]
fn csrf_and_content_type_are_checked_first() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let b = Browser::new();
    let mut req = b.req(
        "email_start",
        "POST",
        "/api/account/v1/email/start",
        Some("application/json"),
        b"{\"email\":\"a@example.com\"}".to_vec(),
    );
    for (fetch_site, origin) in
        [(Some("cross-site"), Some(SITE)), (None, Some("null")), (None, None), (Some("same-site"), Some(SECRET))]
    {
        req.headers.retain(|(k, _)| k != "sec-fetch-site" && k != "origin");
        if let Some(f) = fetch_site {
            req.headers.push(("sec-fetch-site".into(), f.into()));
        }
        if let Some(o) = origin {
            req.headers.push(("origin".into(), o.into()));
        }
        assert_eq!(h.call(&cfg, &req).status, 403, "{fetch_site:?} {origin:?}");
    }
    assert!(h.mail.borrow().is_empty());
    let r = Browser::new().req(
        "email_confirm",
        "POST",
        "/api/account/v1/email/confirm",
        Some("text/plain"),
        b"{}".to_vec(),
    );
    assert_eq!(h.call(&cfg, &r).status, 415);
    // A forwarded request cannot reach an internal route.
    let r = Browser::new().req("me", "POST", "/internal/v1/session", Some("application/json"), b"{}".to_vec());
    assert_eq!(h.call(&cfg, &r).status, 404);
    // A route arriving with the secret host's X-SN-Host is not ours.
    let mut r = Browser::new().req("me", "GET", "/api/account/v1/me", None, vec![]);
    r.headers.retain(|(k, _)| k != "x-sn-host");
    r.headers.push(("x-sn-host".into(), "s.slop.localhost:8788".into()));
    assert_eq!(h.call(&cfg, &r).status, 404);
    let _ = h.random::<4>();
}

/// A form post from a page served with `Referrer-Policy: no-referrer`: the
/// browser says `Sec-Fetch-Site: same-origin` and serializes its `Origin` as
/// `null`. The email sign-in form and a signed-in account form both pass;
/// `null` without Fetch Metadata does not.
#[test]
fn a_form_post_from_a_page_that_sends_no_referrer_passes() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let null_origin = |mut r: slop_ninja_account_worker::http::Req, sfs: Option<&str>| {
        r.headers.retain(|(k, _)| k != "sec-fetch-site" && k != "origin");
        if let Some(f) = sfs {
            r.headers.push(("sec-fetch-site".into(), f.into()));
        }
        r.headers.push(("origin".into(), "null".into()));
        r
    };
    let form = |b: &Browser, addr: &str| {
        let body = url::form_urlencoded::Serializer::new(String::new()).append_pair("email", addr).finish();
        b.req(
            "email_start",
            "POST",
            "/api/account/v1/email/start",
            Some("application/x-www-form-urlencoded"),
            body.into_bytes(),
        )
    };
    // Without Fetch Metadata, `Origin: null` is refused and nothing is sent.
    let mut b = Browser::new();
    let r = b.send(&h, &cfg, null_origin(form(&b, ALICE), None));
    assert_eq!(r.status, 403);
    assert!(h.mails_to(ALICE).is_empty());
    // With it, the sign-in form is sent and answers its page.
    let r = b.send(&h, &cfg, null_origin(form(&b, ALICE), Some("same-origin")));
    assert_eq!(r.status, 303);
    assert_eq!(r.header("location"), Some("https://slop.localhost:8787/signin/check-email/"));
    assert_eq!(h.mails_to(ALICE).len(), 1);
    // A signed-in form (sign out) posted the same way works too.
    let mut signed = Browser::new();
    h.advance(61_000);
    signed.email_sign_in(&h, &cfg, "bob@example.com");
    assert!(signed.signed_in());
    let logout = signed.req("logout", "POST", "/auth/logout", Some("application/x-www-form-urlencoded"), vec![]);
    let r = signed.send(&h, &cfg, null_origin(logout, Some("same-origin")));
    assert_eq!(r.status, 303);
    assert!(!signed.signed_in());
    // Cross-site with `Origin: null` (a sandboxed frame elsewhere) is refused.
    let mut c = Browser::new();
    let r = c.send(&h, &cfg, null_origin(form(&c, ALICE), Some("cross-site")));
    assert_eq!(r.status, 403);
    assert_eq!(h.mails_to(ALICE).len(), 1);
}

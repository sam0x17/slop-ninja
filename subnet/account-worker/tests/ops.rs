//! Signed account ops: the envelope checks, the account and switch ops,
//! `attach_email` refused at launch, and `x_signin_purge` (Δ28) with its
//! dry run, its pages, its content-free journal and a replay after a store
//! restore.

mod support;

use serde_json::{Value, json};
use slop_ninja_account_worker::Req;
use support::*;

fn send_raw(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    body: Vec<u8>,
) -> (u16, Value) {
    let req = Req {
        method: "POST".into(),
        path: "/api/account/v1/op".into(),
        query: String::new(),
        headers: vec![
            ("x-sn-route".into(), "account_op".into()),
            ("x-sn-host".into(), "slop.localhost:8787".into()),
            ("content-type".into(), "application/json".into()),
        ],
        body,
    };
    let r = h.call(cfg, &req);
    (r.status, serde_json::from_slice(&r.body).unwrap_or(Value::Null))
}

#[test]
fn the_envelope_is_checked_and_a_nonce_works_once() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let body = op_body(&h, "set_switch", json!({"name": "auth.email", "state": "off", "reason": "drill"}));
    let (s, a) = send_raw(&h, &cfg, body.clone());
    assert_eq!(s, 200, "{a}");
    let (s, a) = send_raw(&h, &cfg, body);
    assert_eq!((s, a["code"].clone()), (409, json!("op_reused_nonce")));
    // A production Worker refuses a staging op.
    let prod = config(&[("OP_ENV", "production")]);
    let (s, a) =
        send_raw(&h, &prod, op_body(&h, "set_switch", json!({"name": "auth.email", "state": "on", "reason": "x"})));
    assert_eq!((s, a["code"].clone()), (403, json!("op_wrong_env")));
    // A detect op, an unknown switch, a missing reason.
    let (s, a) = op(&h, &cfg, "x_purge", json!({"reason": "r", "dry_run": true}));
    assert_eq!((s, a["code"].clone()), (400, json!("op_unknown")));
    let (s, _) = op(&h, &cfg, "set_switch", json!({"name": "DETECT_TOOL", "state": "off", "reason": "r"}));
    assert_eq!(s, 400);
    let (s, _) = op(&h, &cfg, "set_switch", json!({"name": "auth.email", "state": "off"}));
    assert_eq!(s, 400);
    let mut b = Browser::new();
    let r = b.email_start(&h, &cfg, json!({"email": "reader@example.com"}));
    assert_eq!(r.status, 503, "auth.email is off");
    let (s, _) = op(&h, &cfg, "set_switch", json!({"name": "auth.email", "state": "on", "reason": "drill over"}));
    assert_eq!(s, 200);
    assert_eq!(b.email_start(&h, &cfg, json!({"email": "reader@example.com"})).status, 202);
    // A var's hard off holds whatever the row says.
    let hard = config(&[("auth.email", "off")]);
    assert_eq!(Browser::new().email_start(&h, &hard, json!({"email": "reader@example.com"})).status, 503);
    // A ledger op without its arguments is refused as malformed.
    let (s, a) = op(&h, &cfg, "grant_credit", json!({"reason": "r"}));
    assert_eq!((s, a["code"].clone()), (400, json!("op_malformed")));
    // No key configured: the op route is off.
    let none = config(&[("DETECT_OP_KEY", "")]);
    assert_eq!(op(&h, &none, "set_switch", json!({"reason": "r"})).0, 503);
}

#[test]
fn account_ops_lock_flag_revoke_and_delete_by_recorded_decision() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut a = Browser::new();
    a.email_sign_in(&h, &cfg, "reader@example.com");
    let u = user_of(&mut a, &h, &cfg);
    let (s, _) =
        op(&h, &cfg, "set_flag", json!({"user_id": u, "flag": "paid_preview", "value": true, "reason": "tester"}));
    assert_eq!(s, 200);
    assert_eq!(a.session(&h, &cfg).unwrap()["flags"], 1);
    assert_eq!(a.me(&h, &cfg)["paid_preview"], true);
    let (s, a2) = op(&h, &cfg, "revoke_sessions", json!({"user_id": u, "reason": "lost device"}));
    assert_eq!((s, a2["sessions"].clone()), (200, json!(1)));
    assert!(a.session(&h, &cfg).is_none());
    // Deletion needs the operator's own basis, never a bare claim.
    let (s, _) = op(&h, &cfg, "delete_account", json!({"user_id": u, "reason": "someone says it is theirs"}));
    assert_eq!(s, 400);
    let (s, _) = op(&h, &cfg, "delete_account", json!({"user_id": u, "basis": "claim", "reason": "r"}));
    assert_eq!(s, 400);
    let (s, ans) = op(&h, &cfg, "delete_account", json!({"user_id": u, "basis": "legal", "reason": "court order"}));
    assert_eq!(s, 200, "{ans}");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users WHERE state = 'deleted'"), 1);
    let row = &h.q("SELECT actor, op, signature, nonce_h FROM journal ORDER BY seq DESC")[0];
    assert!(row.text("actor").unwrap().starts_with("operator:"));
    assert_eq!(row.blob("signature").unwrap().len(), 64);
    assert!(row.text("op").unwrap().contains("\"basis\":\"legal\""));
    // Recovery by someone's word is refused at launch, and journaled.
    let (s, a3) =
        op(&h, &cfg, "attach_email", json!({"user_id": u, "email": "new@example.org", "reason": "says it is theirs"}));
    assert_eq!((s, a3["code"].clone()), (403, json!("no_accepted_evidence")));
    let row = &h.q("SELECT op FROM journal ORDER BY seq DESC")[0];
    assert!(row.text("op").unwrap().contains("no_accepted_evidence"));
    assert!(!row.text("op").unwrap().contains("new@example.org"));
}

/// An account with an X identity (and, when `other`, an address too),
/// with an X session and an avatar.
fn x_account(
    h: &TestHost,
    cfg: &Result<slop_ninja_account_worker::Config, slop_ninja_account_worker::ConfigError>,
    user: &str,
    other: Option<&str>,
) -> Browser {
    h.stub.borrow_mut().x.user = Some(user.into());
    let mut b = Browser::new();
    assert_eq!(b.oauth(h, cfg, "x", "").status, 303);
    if let Some(addr) = other {
        h.advance(61_000);
        b.email_start(h, cfg, json!({"email": addr, "intent": "link"}));
        let t = h.mails_to(addr).last().unwrap().token();
        b.email_confirm(h, cfg, &t);
        assert_eq!(b.post_form(h, cfg, "link_confirm", "/account/link/confirm", &[("action", "connect")]).status, 303);
    }
    b
}

#[test]
fn x_signin_purge_deletes_what_x_sign_in_stored_and_nothing_else() {
    let h = TestHost::new();
    let cfg = config(&[]);
    // Alice and Bob have X and an address; Carol, Dave and Erin X only.
    let mut alice = x_account(&h, &cfg, "alice", Some("alice@example.com"));
    let mut bob = x_account(&h, &cfg, "bob", Some("bob@example.com"));
    for u in ["carol", "dave", "erin"] {
        x_account(&h, &cfg, u, None);
    }
    let mut alice_mail = Browser::new();
    alice_mail.email_sign_in(&h, &cfg, "alice@example.com");
    let carol = h.q("SELECT hex(user_id) AS u FROM identities WHERE handle = 'carol_example'")[0]
        .text("u")
        .unwrap()
        .to_lowercase();
    // The dry run counts and writes nothing but its journal row.
    let before = h.dump();
    let (s, dry) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": true, "reason": "agreement ends"}));
    assert_eq!(s, 200, "{dry}");
    assert_eq!(dry["identities"], 5);
    assert_eq!(dry["x_only_accounts"], 3);
    assert_eq!(dry["avatars"], 2, "alice's and bob's (the stub serves the others' as non-images)");
    let after = h.dump();
    let strip = |d: &str| d.lines().filter(|l| !l.starts_with("journal.")).collect::<Vec<_>>().join("\n");
    assert_eq!(strip(&before), strip(&after));
    // The purge.
    let (s, ans) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": false, "reason": "agreement ends"}));
    assert_eq!(s, 200, "{ans}");
    assert_eq!(ans["remaining"], 0);
    assert_eq!(ans["accounts_deleted"], 3);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'x'"), 0);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM avatars"), 0);
    assert!(alice.session(&h, &cfg).is_none() && bob.session(&h, &cfg).is_none(), "the X sessions are revoked");
    assert!(alice_mail.session(&h, &cfg).is_some(), "other sessions are not");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM users WHERE state = 'deleted'"), 3);
    let purges: Vec<Value> = h
        .queue
        .borrow()
        .iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .filter(|v| v["type"] == "owner_purge")
        .collect();
    assert_eq!(purges.len(), 3);
    assert!(purges.iter().all(|p| p["mode"] == "release"));
    assert!(purges.iter().any(|p| p["user_id"] == carol));
    let (_, dry) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": true, "reason": "closing check"}));
    for k in ["identities", "avatars", "sessions", "x_only_accounts", "pending"] {
        assert_eq!(dry[k], 0, "{k}");
    }
    // The journal names no X user: no id, handle or address.
    let journal = h.q("SELECT op, reason FROM journal");
    for r in &journal {
        let op = r.text("op").unwrap();
        for leak in ["1900000000000000", "_example", "@example."] {
            assert!(!op.contains(leak), "{op}");
        }
    }
}

#[test]
fn x_signin_purge_works_in_pages_and_replays_after_a_restore() {
    let h = TestHost::new();
    let cfg = config(&[]);
    // More X identities than one page holds, written straight into the store.
    let n = slop_ninja_account_worker::ops::X_PURGE_PAGE as i64 + 7;
    {
        let db = h.db.borrow();
        for i in 0..n {
            let u = vec![i as u8 + 1; 16];
            db.execute("INSERT INTO users (user_id, state, created_at) VALUES (?, 'active', 1)", [u.clone()]).unwrap();
            db.execute(
                "INSERT INTO identities (user_id, provider, subject, handle, created_at, last_used_at) VALUES (?, 'x', ?, ?, 1, 1)",
                rusqlite::params![u.clone(), format!("{}", 1_000 + i), format!("h{i}")],
            )
            .unwrap();
            db.execute(
                "INSERT INTO identities (user_id, provider, subject, created_at, last_used_at) VALUES (?, 'email', ?, 1, 1)",
                rusqlite::params![u, format!("u{i}@example.org")],
            )
            .unwrap();
        }
    }
    // Saved before the op: the store a point-in-time restore brings back.
    let snapshot = h.dump();
    let mut backup = rusqlite::Connection::open_in_memory().unwrap();
    {
        let db = h.db.borrow();
        let b = rusqlite::backup::Backup::new(&db, &mut backup).unwrap();
        b.step(-1).unwrap();
    }
    let (_, first) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": false, "reason": "agreement ends"}));
    assert_eq!(first["remaining"], 7, "{first}");
    let (_, second) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": false, "reason": "agreement ends"}));
    assert_eq!(second["remaining"], 0, "{second}");
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'x'"), 0);
    // Restore: the X identities are back; replay re-signs the purge until
    // its dry run counts zero.
    {
        let mut db = h.db.borrow_mut();
        let b = rusqlite::backup::Backup::new(&backup, &mut db).unwrap();
        b.step(-1).unwrap();
    }
    assert_eq!(h.dump(), snapshot);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'x'"), n);
    loop {
        let (_, dry) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": true, "reason": "replay"}));
        if dry["identities"] == 0 {
            break;
        }
        let (s, _) = op(&h, &cfg, "x_signin_purge", json!({"dry_run": false, "reason": "replay"}));
        assert_eq!(s, 200);
    }
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'x'"), 0);
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM identities WHERE provider = 'email'"), n);
}

/// An op signed for another Worker, out of its window, or with anything
/// beside the envelope is refused before it runs (Δ6).
#[test]
fn ops_for_another_target_or_window_are_refused() {
    use slop_ninja_account_worker::host::Host as _;
    use slop_ninja_signed_op::{Env, Nonce, Op, Target};
    let h = TestHost::new();
    let cfg = config(&[]);
    let signed = |target: Target, issued: u64, expires: u64, name: &str| {
        let n: [u8; 16] = h.random();
        let op = Op {
            target,
            env: Env::Staging,
            issued_ms: issued,
            expires_ms: expires,
            nonce: Nonce(n),
            op: name.into(),
            args: json!({"reason": "r", "name": "auth.email", "state": "off"}).as_object().unwrap().clone(),
        };
        serde_json::to_value(slop_ninja_signed_op::sign(&op, &op_key()).unwrap()).unwrap()
    };
    let now = h.now.get();
    let detect = signed(Target::Detect, now, now + 60_000, "set_switch");
    let (s, a) = send_raw(&h, &cfg, serde_json::to_vec(&detect).unwrap());
    assert_eq!((s, a["code"].clone()), (403, json!("op_wrong_target")));
    let edge = signed(Target::Edge, now, now + 60_000, "edge_coverage");
    assert_eq!(send_raw(&h, &cfg, serde_json::to_vec(&edge).unwrap()).1["code"], json!("op_wrong_target"));
    let old = signed(Target::Account, now - 120_000, now - 60_000, "set_switch");
    assert_eq!(send_raw(&h, &cfg, serde_json::to_vec(&old).unwrap()).1["code"], json!("op_expired"));
    let ahead = signed(Target::Account, now + 600_000, now + 660_000, "set_switch");
    assert_eq!(send_raw(&h, &cfg, serde_json::to_vec(&ahead).unwrap()).1["code"], json!("op_not_yet_valid"));
    let mut beside = signed(Target::Account, now, now + 60_000, "set_switch");
    beside["x_url"] = json!("https://x.com/i/status/2");
    assert_eq!(send_raw(&h, &cfg, serde_json::to_vec(&beside).unwrap()).1["code"], json!("op_malformed"));
    assert_eq!(h.count("SELECT COUNT(*) AS n FROM journal"), 0, "nothing ran");
}

/// The detect Worker's `owner_match` reads every account id, in id order,
/// a page at a time, through this binding-only route.
#[test]
fn the_users_route_pages_every_account_id() {
    let h = TestHost::new();
    let cfg = config(&[]);
    let mut ids = Vec::new();
    for addr in ["a@example.com", "b@example.com", "c@example.com"] {
        let mut b = Browser::new();
        b.email_sign_in(&h, &cfg, addr);
        ids.push(user_of(&mut b, &h, &cfg));
    }
    ids.sort();
    let (s, _) = op(&h, &cfg, "disable_account", json!({"user_id": ids[1], "reason": "abuse"}));
    assert_eq!(s, 200);
    let mut b = Browser::new();
    let page = |b: &mut Browser, body: Value| {
        let r = b.internal(&h, &cfg, "/internal/v1/users", body);
        (r.status, serde_json::from_slice::<Value>(&r.body).unwrap_or(Value::Null))
    };
    let (s, a) = page(&mut b, json!({"after": null, "limit": 2}));
    assert_eq!(s, 200);
    let got: Vec<&str> = a["users"].as_array().unwrap().iter().map(|u| u["user_id"].as_str().unwrap()).collect();
    assert_eq!(got, [ids[0].as_str(), ids[1].as_str()]);
    assert_eq!(a["users"][1]["state"], "locked");
    assert_eq!(a["next"], json!(ids[1]));
    let (_, a) = page(&mut b, json!({"after": ids[1], "limit": 2}));
    assert_eq!((a["users"][0]["user_id"].clone(), a["next"].clone()), (json!(ids[2]), Value::Null));
    for bad in [json!({"after": null, "limit": 0}), json!({"after": null, "limit": 501}), json!({"limit": 1, "x": 1})] {
        assert_eq!(page(&mut b, bad).0, 400);
    }
    // A forwarded request never reaches it.
    let req = Req {
        method: "POST".into(),
        path: "/internal/v1/users".into(),
        query: String::new(),
        headers: vec![("x-sn-route".into(), "me".into()), ("content-type".into(), "application/json".into())],
        body: br#"{"after":null,"limit":2}"#.to_vec(),
    };
    assert_eq!(h.call(&cfg, &req).status, 404);
}

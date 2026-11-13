//! The account store's migration (`subnet/account-worker/migrations/`)
//! applied to an in-memory SQLite with foreign keys on, as D1 runs it: every
//! CHECK and UNIQUE is exercised, the CHECK lists equal this crate's value
//! sets, and the ledger's batches behave as the design says (a batch is one
//! transaction; a failing statement rolls back all of it).

use rusqlite::types::Value as V;
use rusqlite::{Connection, params_from_iter};
use slop_ninja_account_wire::*;

const SCHEMA: &str = include_str!("../../../account-worker/migrations/0001_account_store.sql");

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.pragma_update(None, "foreign_keys", true).unwrap();
    c.execute_batch(SCHEMA).unwrap();
    c
}

fn run(c: &Connection, sql: &str, p: &[V]) -> Result<usize, String> {
    c.execute(sql, params_from_iter(p.iter())).map_err(|e| e.to_string())
}

fn refused(c: &Connection, sql: &str, p: &[V], what: &str) {
    let e = run(c, sql, p).expect_err(sql);
    assert!(e.contains(what), "{sql}: expected {what}, got {e}");
}

/// A D1 batch: every statement in one transaction, all or nothing.
fn batch(c: &mut Connection, stmts: &[(&str, Vec<V>)]) -> Result<(), String> {
    let tx = c.transaction().map_err(|e| e.to_string())?;
    for (sql, p) in stmts {
        tx.execute(sql, params_from_iter(p.iter())).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn b(x: u8) -> V {
    V::Blob(vec![x; 16])
}
fn t(s: &str) -> V {
    V::Text(s.into())
}
fn i(n: i64) -> V {
    V::Integer(n)
}
const NULL: V = V::Null;
/// The state a new hold takes.
const HOLDING: &str = "held";

fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

fn in_lists(col: &str) -> Vec<Vec<String>> {
    let needle = format!("CHECK ({col} IN (");
    let mut out = Vec::new();
    let mut rest = SCHEMA;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        let end = after.find(')').unwrap();
        out.push(after[..end].split(',').map(|s| s.trim().trim_matches('\'').to_owned()).collect());
        rest = &after[end..];
    }
    assert!(!out.is_empty(), "no IN list for {col}");
    out
}

fn strs<T: Copy>(all: &[T], s: impl Fn(T) -> &'static str) -> Vec<String> {
    all.iter().map(|x| s(*x).to_owned()).collect()
}

#[test]
fn check_lists_equal_the_value_sets() {
    assert_eq!(in_lists("provider"), vec![strs(Provider::ALL, Provider::as_str)]);
    assert_eq!(in_lists("content_type"), vec![strs(AvatarType::ALL, AvatarType::as_str)]);
    // intent: auth_flows, email_tokens, wallet_chals.
    assert_eq!(
        in_lists("intent"),
        vec![
            strs(Intent::ALL, Intent::as_str),
            strs(MailIntent::ALL, MailIntent::as_str),
            strs(Intent::ALL, Intent::as_str)
        ]
    );
    // kind: pending_links, payments, hold_ends.
    assert_eq!(
        in_lists("kind"),
        vec![
            strs(PendingKind::ALL, PendingKind::as_str),
            strs(PaymentKind::ALL, PaymentKind::as_str),
            strs(HoldEnd::ALL, HoldEnd::as_str)
        ]
    );
    // state: users, payments, plan_periods, holds.
    assert_eq!(
        in_lists("state"),
        vec![
            strs(UserState::ALL, UserState::as_str),
            strs(PaymentState::ALL, PaymentState::as_str),
            strs(PeriodState::ALL, PeriodState::as_str),
            strs(HoldState::ALL, HoldState::as_str),
        ]
    );
}

#[test]
fn schema_info_names_the_schema() {
    let c = db();
    let v: String = c.query_row("SELECT v FROM schema_info WHERE k = 'schema'", [], |r| r.get(0)).unwrap();
    assert_eq!(v, "slop_ninja_account_store_v1");
}

const USER: &str = "INSERT INTO users (user_id, state, created_at, flags) VALUES (?, ?, 1, ?)";
const IDENT: &str = "INSERT INTO identities (identity_id, user_id, provider, subject, created_at, last_used_at) \
                     VALUES (?, ?, ?, ?, 1, 1)";

#[test]
fn users_identities_and_avatars() {
    let c = db();
    run(&c, USER, &[b(1), t("active"), i(0)]).unwrap();
    run(&c, USER, &[b(2), t("locked"), i(1)]).unwrap();
    refused(&c, USER, &[b(3), t("banned"), i(0)], "CHECK constraint failed");
    refused(&c, USER, &[b(4), t("active"), i(-1)], "CHECK constraint failed");
    refused(&c, USER, &[b(1), t("active"), i(0)], "UNIQUE constraint failed: users.user_id");
    run(&c, IDENT, &[i(1), b(1), t("x"), t("12345")]).unwrap();
    refused(
        &c,
        IDENT,
        &[i(2), b(2), t("x"), t("12345")],
        "UNIQUE constraint failed: identities.provider, identities.subject",
    );
    // The same subject under another provider is another identity.
    run(&c, IDENT, &[i(3), b(2), t("github"), t("12345")]).unwrap();
    refused(&c, IDENT, &[i(4), b(1), t("apple"), t("a")], "CHECK constraint failed");
    refused(&c, IDENT, &[i(5), b(9), t("google"), t("g")], "FOREIGN KEY constraint failed");
    let avatar = "INSERT INTO avatars (identity_id, ref, content_type, bytes, fetched_at) VALUES (?, ?, ?, x'00', 1)";
    run(&c, avatar, &[i(1), b(7), t("image/png")]).unwrap();
    refused(&c, avatar, &[i(3), b(7), t("image/png")], "UNIQUE constraint failed: avatars.ref");
    refused(&c, avatar, &[i(3), b(8), t("image/svg+xml")], "CHECK constraint failed");
    refused(&c, avatar, &[i(99), b(9), t("image/gif")], "FOREIGN KEY constraint failed");
    // An identity with an avatar is deleted after its avatar.
    refused(&c, "DELETE FROM identities WHERE identity_id = 1", &[], "FOREIGN KEY constraint failed");
    run(&c, "DELETE FROM avatars WHERE identity_id = 1", &[]).unwrap();
    run(&c, "DELETE FROM identities WHERE identity_id = 1", &[]).unwrap();
}

#[test]
fn flows_bind_intent_and_user() {
    let c = db();
    let flow = "INSERT INTO auth_flows (state_h, binding_h, provider, intent, user_id, session_h, pkce_verifier, \
                return_to, created_at, expires_at) VALUES (?, ?, 'google', ?, ?, ?, 'v', '/account/', 1, 2)";
    run(&c, flow, &[b(1), b(1), t("signin"), NULL, NULL]).unwrap();
    run(&c, flow, &[b(2), b(1), t("link"), b(5), b(6)]).unwrap();
    run(&c, flow, &[b(3), b(1), t("step_up"), b(5), b(6)]).unwrap();
    refused(&c, flow, &[b(4), b(1), t("signin"), b(5), NULL], "CHECK constraint failed");
    refused(&c, flow, &[b(5), b(1), t("link"), NULL, NULL], "CHECK constraint failed");
    refused(&c, flow, &[b(6), b(1), t("attach_pending"), NULL, NULL], "CHECK constraint failed");
    let chal = "INSERT INTO wallet_chals (chal_h, binding_h, pubkey, message, intent, return_to, created_at, \
                expires_at) VALUES (?, ?, ?, x'00', ?, '/detect/', ?, ?) ON CONFLICT (binding_h, pubkey) DO UPDATE \
                SET chal_h = excluded.chal_h, message = excluded.message, intent = excluded.intent, \
                return_to = excluded.return_to, created_at = excluded.created_at, expires_at = excluded.expires_at";
    run(&c, chal, &[b(1), b(1), b(9), t("signin"), i(1), i(300)]).unwrap();
    // A newer challenge from the same browser for the same key replaces it,
    // with its own destination.
    run(&c, &chal.replace("'/detect/'", "'/d/k7m2xq9'"), &[b(2), b(1), b(9), t("signin"), i(2), i(301)]).unwrap();
    let to: String = c
        .query_row("SELECT return_to FROM wallet_chals WHERE chal_h = x'02020202020202020202020202020202'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(to, "/d/k7m2xq9");
    // A challenge always stores where its flow goes.
    refused(
        &c,
        "INSERT INTO wallet_chals (chal_h, binding_h, pubkey, message, intent, return_to, created_at, expires_at) \
         VALUES (?, ?, ?, x'00', 'signin', NULL, 1, 2)",
        &[b(5), b(5), b(9)],
        "NOT NULL constraint failed: wallet_chals.return_to",
    );
    // Other browsers' challenges for that key never touch it.
    for n in 10..40u8 {
        run(&c, chal, &[b(n), b(n), b(9), t("signin"), i(3), i(302)]).unwrap();
    }
    assert_eq!(one(&c, "SELECT count(*) FROM wallet_chals WHERE binding_h = x'01010101010101010101010101010101'"), 1);
    assert_eq!(one(&c, "SELECT count(*) FROM wallet_chals WHERE chal_h = x'02020202020202020202020202020202'"), 1);
    refused(&c, chal, &[b(3), b(2), b(8), t("attach_pending"), i(1), i(2)], "CHECK constraint failed");
    // The plain insert of a second challenge for (browser, key) is refused.
    refused(
        &c,
        "INSERT INTO wallet_chals (chal_h, binding_h, pubkey, message, intent, return_to, created_at, expires_at) \
         VALUES (?, ?, ?, x'00', 'signin', '/detect/', 1, 2)",
        &[b(4), b(1), b(9)],
        "UNIQUE constraint failed: wallet_chals.binding_h, wallet_chals.pubkey",
    );
}

#[test]
fn email_links_are_per_address_and_browser() {
    let c = db();
    // The conditional upsert of a link mail: a newer link from this browser replaces
    // this browser's older one only after the spacing; other browsers keep
    // their own rows.
    let upsert = "INSERT INTO email_tokens (token_h, addr_h, email_norm, email_typed, intent, binding_h, return_to, \
                  created_at, expires_at) VALUES (?, ?, 'kate@host.example', 'Kate@host.example', ?, ?, '/', ?, ?) \
                  ON CONFLICT (addr_h, binding_h) DO UPDATE SET token_h = excluded.token_h, intent = excluded.intent, \
                  created_at = excluded.created_at, expires_at = excluded.expires_at \
                  WHERE email_tokens.created_at <= ? RETURNING token_h";
    let send = |c: &Connection, token: u8, binding: u8, intent: &str, now: i64| -> usize {
        let p = [b(token), b(1), t(intent), b(binding), i(now), i(now + 900_000), i(now - 60_000)];
        let mut s = c.prepare(upsert).unwrap();
        s.query_map(params_from_iter(p.iter()), |r| r.get::<_, Vec<u8>>(0)).unwrap().count()
    };
    assert_eq!(send(&c, 1, 1, "signin", 1_000_000), 1);
    // Within the spacing, from the same browser: nothing is sent.
    assert_eq!(send(&c, 2, 1, "signin", 1_030_000), 0);
    // Five links for the same address from another browser, and a link
    // intent: none touches browser 1's row.
    for n in 0..5u8 {
        assert_eq!(send(&c, 10 + n, 2, "signin", 1_000_000 + i64::from(n) * 61_000), 1);
    }
    assert_eq!(send(&c, 20, 3, "link", 1_000_000), 1);
    assert_eq!(one(&c, "SELECT count(*) FROM email_tokens WHERE token_h = x'01010101010101010101010101010101'"), 1);
    // After the spacing, browser 1's next link replaces its own.
    assert_eq!(send(&c, 3, 1, "signin", 1_061_000), 1);
    assert_eq!(one(&c, "SELECT count(*) FROM email_tokens WHERE binding_h = x'01010101010101010101010101010101'"), 1);
    assert_eq!(one(&c, "SELECT count(*) FROM email_tokens"), 3);
    let plain = "INSERT INTO email_tokens (token_h, addr_h, email_norm, email_typed, intent, binding_h, return_to, \
                 created_at, expires_at) VALUES (?, ?, 'a@b.example', 'a@b.example', ?, ?, '/', 1, 2)";
    refused(
        &c,
        plain,
        &[b(33), b(1), t("signin"), b(1)],
        "UNIQUE constraint failed: email_tokens.addr_h, email_tokens.binding_h",
    );
    refused(&c, plain, &[b(31), b(5), t("magic"), b(5)], "CHECK constraint failed");
    for m in MailIntent::ALL {
        run(&c, plain, &[V::Blob(vec![40 + *m as u8; 16]), b(6), t(m.as_str()), V::Blob(vec![40 + *m as u8; 16])])
            .unwrap();
    }
}

#[test]
fn pending_links_and_owned() {
    let c = db();
    let pending = "INSERT INTO pending_links (binding_h, kind, user_id, session_h, provider, subject, email_verified, \
                   from_user_id, return_to, created_at, expires_at) VALUES (?, ?, ?, ?, 'x', 's', 0, ?, '/account/', 1, 9)";
    run(&c, pending, &[b(1), t("link"), b(5), b(6), NULL]).unwrap();
    run(&c, pending, &[b(2), t("email_in_use"), b(5), NULL, NULL]).unwrap();
    refused(&c, pending, &[b(3), t("attach"), b(5), NULL, NULL], "CHECK constraint failed");
    refused(&c, pending, &[b(1), t("merge"), b(5), b(6), b(7)], "UNIQUE constraint failed");
    // The link confirm takes only link and merge rows, of its own session.
    let confirm = "DELETE FROM pending_links WHERE binding_h = ? AND user_id = ? AND session_h = ? AND expires_at > ? \
                   AND kind IN ('link','merge') RETURNING provider";
    let taken = |c: &Connection, binding: u8, session: V| -> usize {
        let p = [b(binding), b(5), session, i(2)];
        let mut s = c.prepare(confirm).unwrap();
        s.query_map(params_from_iter(p.iter()), |r| r.get::<_, String>(0)).unwrap().count()
    };
    assert_eq!(taken(&c, 2, NULL), 0);
    assert_eq!(taken(&c, 1, b(7)), 0);
    assert_eq!(taken(&c, 1, b(6)), 1);
    assert_eq!(taken(&c, 1, b(6)), 0);
    let owned = "INSERT INTO owned (user_id, det_id_h, enc_ref, is_owner, created_at) VALUES (?, ?, x'00', ?, 1)";
    run(&c, owned, &[b(1), V::Blob(vec![9; 32]), i(1)]).unwrap();
    refused(&c, owned, &[b(1), V::Blob(vec![9; 32]), i(0)], "UNIQUE constraint failed: owned.user_id, owned.det_id_h");
    run(&c, owned, &[b(2), V::Blob(vec![9; 32]), i(0)]).unwrap();
    let journal = "INSERT INTO journal (at, actor, op, nonce_h) VALUES (1, 'user', '{}', ?)";
    run(&c, journal, &[b(1)]).unwrap();
    run(&c, journal, &[NULL]).unwrap();
    run(&c, journal, &[NULL]).unwrap();
    refused(&c, journal, &[b(1)], "UNIQUE constraint failed: journal.nonce_h");
}

const PAY: &str = "INSERT INTO payments (payment_id, user_id, kind, cash, tokens_total, tokens_left, processor_ref, state, \
                   created_at, journal_seq) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1)";
const PERIOD: &str = "INSERT INTO plan_periods (period_id, user_id, payment_id, starts_at, ends_at, allowance, api_cap, \
                      allowance_granted, api_cap_granted, used, api_used, cash, state) \
                      VALUES (?, ?, 1, 0, 100, ?, ?, ?, ?, ?, ?, 0, ?)";

#[test]
fn ledger_checks() {
    let c = db();
    run(&c, PAY, &[i(1), b(1), t("grant"), i(0), i(100), i(100), NULL, t("active"), i(1)]).unwrap();
    refused(&c, PAY, &[i(2), b(1), t("gift"), i(0), i(1), i(1), NULL, t("active"), i(1)], "CHECK constraint failed");
    refused(&c, PAY, &[i(3), b(1), t("grant"), i(0), i(1), i(-1), NULL, t("active"), i(1)], "CHECK constraint failed");
    refused(&c, PAY, &[i(4), b(1), t("grant"), i(0), i(1), i(1), NULL, t("paid"), i(1)], "CHECK constraint failed");
    // A closed source has no headroom.
    refused(
        &c,
        PAY,
        &[i(5), b(1), t("processor"), i(1), i(10), i(3), t("o1"), t("refunded"), i(1)],
        "CHECK constraint failed",
    );
    run(&c, PAY, &[i(6), b(1), t("processor"), i(1), i(10), i(0), t("o1"), t("refunded"), i(1)]).unwrap();
    refused(
        &c,
        PAY,
        &[i(7), b(1), t("processor"), i(1), i(10), i(10), t("o1"), t("active"), i(1)],
        "payments.processor_ref",
    );
    run(&c, PAY, &[i(8), b(1), t("grant"), i(0), i(10), i(10), NULL, t("active"), i(1)]).unwrap();
    for (n, k) in PaymentKind::ALL.iter().enumerate() {
        run(&c, PAY, &[i(20 + n as i64), b(2), t(k.as_str()), i(0), i(1), i(1), NULL, t("active"), i(1)]).unwrap();
    }
    run(&c, PERIOD, &[i(1), b(1), i(100), i(40), i(100), i(40), i(0), i(0), t("active")]).unwrap();
    refused(
        &c,
        PERIOD,
        &[i(2), b(1), i(100), i(40), i(100), i(40), i(101), i(0), t("active")],
        "CHECK constraint failed",
    );
    refused(
        &c,
        PERIOD,
        &[i(3), b(1), i(100), i(40), i(100), i(40), i(-1), i(0), t("active")],
        "CHECK constraint failed",
    );
    refused(
        &c,
        PERIOD,
        &[i(4), b(1), i(100), i(40), i(100), i(40), i(0), i(41), t("active")],
        "CHECK constraint failed",
    );
    refused(
        &c,
        PERIOD,
        &[i(5), b(1), i(100), i(40), i(100), i(40), i(0), i(0), t("paused")],
        "CHECK constraint failed",
    );
    refused(
        &c,
        PERIOD,
        &[i(6), b(1), i(100), i(40), i(100), i(40), i(10), i(0), t("ended")],
        "CHECK constraint failed",
    );
    run(&c, PERIOD, &[i(7), b(1), i(10), i(0), i(100), i(40), i(10), i(0), t("ended")]).unwrap();
    let hold = "INSERT INTO holds (hold_id, user_id, channel, tokens, parts, idem, state, created_at, updated_at) \
                VALUES (?, ?, 'paid_page', 5, '[]', ?, ?, 1, 1)";
    run(&c, hold, &[t("h1"), b(1), t("q1"), t(HOLDING)]).unwrap();
    refused(&c, hold, &[t("h2"), b(1), t("q1"), t(HOLDING)], "UNIQUE constraint failed: holds.idem");
    refused(&c, hold, &[t("h3"), b(1), t("q3"), t("pending")], "CHECK constraint failed");
    refused(&c, hold, &[t("h1"), b(1), t("q4"), t(HOLDING)], "UNIQUE constraint failed: holds.hold_id");
    let end = "INSERT INTO hold_ends (hold_id, kind, at) VALUES (?, ?, 1)";
    refused(&c, end, &[t("h1"), t("voided")], "CHECK constraint failed");
    run(&c, end, &[t("h1"), t("committed")]).unwrap();
    refused(&c, end, &[t("h1"), t("released")], "UNIQUE constraint failed: hold_ends.hold_id");
    let debit = "INSERT INTO debits (hold_id, user_id, src, src_id, tokens, cash, package_pub, receipt_id, channel, at) \
                 VALUES ('h1', ?, ?, ?, 5, 0, 'p', 'r', 'paid_page', 1)";
    run(&c, debit, &[b(1), t("payment"), i(1)]).unwrap();
    run(&c, debit, &[b(1), t("period"), i(1)]).unwrap();
    refused(
        &c,
        debit,
        &[b(1), t("payment"), i(1)],
        "UNIQUE constraint failed: debits.hold_id, debits.src, debits.src_id",
    );
}

/// A hold batch: unguarded decrements plus the hold row. An
/// overdraw breaks a CHECK and rolls back the whole batch, hold included.
fn hold_batch(c: &mut Connection, hold: &str, idem: &str, from_payment: i64, from_period: i64) -> Result<(), String> {
    batch(
        c,
        &[
            ("UPDATE plan_periods SET used = used + ? WHERE period_id = 1", vec![i(from_period)]),
            ("UPDATE payments SET tokens_left = tokens_left - ? WHERE payment_id = 1", vec![i(from_payment)]),
            (
                "INSERT INTO holds (hold_id, user_id, channel, tokens, parts, idem, state, created_at, updated_at) \
                 VALUES (?, ?, 'paid_page', ?, '[]', ?, ?, 1, 1)",
                vec![t(hold), b(1), i(from_payment + from_period), t(idem), t(HOLDING)],
            ),
        ],
    )
}

#[test]
fn hold_batches_roll_back_whole() {
    let mut c = db();
    run(&c, PAY, &[i(1), b(1), t("grant"), i(0), i(300), i(300), NULL, t("active"), i(1)]).unwrap();
    run(&c, PERIOD, &[i(1), b(1), i(100), i(40), i(100), i(40), i(0), i(0), t("active")]).unwrap();
    // Plan first, then credit.
    hold_batch(&mut c, "h1", "q1", 200, 100).unwrap();
    assert_eq!((one(&c, "SELECT tokens_left FROM payments"), one(&c, "SELECT used FROM plan_periods")), (100, 100));
    // Twenty holds of 25, each planned against the same stale read of 100
    // and written as unguarded decrements: the CHECKs let exactly four in.
    let mut placed = 0;
    for n in 0..20 {
        if hold_batch(&mut c, &format!("r{n}"), &format!("qr{n}"), 25, 0).is_ok() {
            placed += 1;
        }
    }
    assert_eq!(placed, 4);
    assert_eq!(one(&c, "SELECT tokens_left FROM payments"), 0);
    assert_eq!(one(&c, "SELECT count(*) FROM holds"), 5);
    // The plan's allowance is spent: a draw on it rolls back, hold included.
    assert!(hold_batch(&mut c, "h2", "q2", 0, 1).unwrap_err().contains("CHECK constraint failed"));
    // A duplicate idem rolls back the decrements with it.
    assert!(hold_batch(&mut c, "h3", "q1", 0, 0).unwrap_err().contains("holds.idem"));
    assert_eq!(one(&c, "SELECT used FROM plan_periods"), 100);
    assert_eq!(one(&c, "SELECT count(*) FROM holds"), 5);
    // A closed source: a refund zeroes it, and a late decrement aborts.
    run(&c, "UPDATE payments SET state = 'refunded', tokens_left = 0 WHERE payment_id = 1", &[]).unwrap();
    assert!(hold_batch(&mut c, "h4", "q4", 1, 0).unwrap_err().contains("CHECK constraint failed"));
    // A release returns nothing to a closed source.
    assert_eq!(
        run(&c, "UPDATE payments SET tokens_left = tokens_left + 25 WHERE payment_id = 1 AND state = 'active'", &[])
            .unwrap(),
        0
    );
}

#[test]
fn a_hold_ends_once() {
    let mut c = db();
    run(&c, PAY, &[i(1), b(1), t("grant"), i(0), i(100), i(75), NULL, t("active"), i(1)]).unwrap();
    run(
        &c,
        "INSERT INTO holds (hold_id, user_id, channel, tokens, parts, idem, state, created_at, updated_at) \
         VALUES ('h1', ?, 'paid_page', 25, '[]', 'q1', ?, 1, 1)",
        &[b(1), t(HOLDING)],
    )
    .unwrap();
    let commit = |c: &mut Connection| {
        batch(
            c,
            &[
                ("INSERT INTO hold_ends (hold_id, kind, at) VALUES ('h1', 'committed', 2)", vec![]),
                (
                    "INSERT INTO debits (hold_id, user_id, src, src_id, tokens, cash, package_pub, receipt_id, channel, at) \
                     VALUES ('h1', ?, 'payment', 1, 25, 0, 'p', 'r', 'paid_page', 2)",
                    vec![b(1)],
                ),
                ("UPDATE holds SET state = 'committed' WHERE hold_id = 'h1'", vec![]),
            ],
        )
    };
    let release = |c: &mut Connection| {
        batch(
            c,
            &[
                ("INSERT INTO hold_ends (hold_id, kind, at) VALUES ('h1', 'released', 3)", vec![]),
                (
                    "UPDATE payments SET tokens_left = tokens_left + 25 WHERE payment_id = 1 AND state = 'active'",
                    vec![],
                ),
                ("UPDATE holds SET state = 'released' WHERE hold_id = 'h1'", vec![]),
            ],
        )
    };
    commit(&mut c).unwrap();
    // A second commit and a release after the commit both fail whole.
    assert!(commit(&mut c).is_err());
    assert!(release(&mut c).is_err());
    assert_eq!(one(&c, "SELECT tokens_left FROM payments"), 75);
    assert_eq!(one(&c, "SELECT count(*) FROM debits"), 1);
    let ended: String = c.query_row("SELECT kind FROM hold_ends WHERE hold_id = 'h1'", [], |r| r.get(0)).unwrap();
    assert_eq!(ended, "committed");
}

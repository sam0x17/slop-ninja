//! Golden JSON fixtures of the account contracts, kept with the other page
//! fixtures under `subnet/crates/detect-page/tests/fixtures/account/`: every
//! account page's view, the session and `me` answers, the saved-link and
//! billing routes and the sign-in API bodies. Built here from typed values,
//! round-tripped through serde and compared byte for byte. Every address and
//! handle is synthetic, under the reserved `.example` domain.
//!
//! To rewrite the files after a deliberate contract change:
//! `SLOP_NINJA_BLESS=1 cargo test --test golden`.

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use slop_ninja_account_wire::detect_page::internal::Summary;
use slop_ninja_account_wire::detect_page::{Band, Channel, Kind};
use slop_ninja_account_wire::*;

/// 2026-11-20T10:04:00Z.
const T0: u64 = 1_795_169_040_000;
const DAY: u64 = 86_400_000;
/// A placeholder in the form of an SS58 address (not a valid one).
const SS58: &str = "5Fixture000000000000000000000000000000000000000";

fn user() -> UserId {
    UserId([0x5a; 16])
}

fn summary(kind: Kind, band: Option<Band>, words: u32) -> Summary {
    Summary {
        kind,
        band,
        short: None,
        strong_sections: None,
        total_sections: None,
        words,
        created_at: T0,
        expires_at: None,
    }
}

fn identity(id: u64, provider: Provider, label: &str, current: bool) -> IdentityRow {
    IdentityRow {
        identity_id: id,
        provider,
        label: label.into(),
        email_verified: matches!(provider, Provider::Email | Provider::Google | Provider::Github),
        avatar_ref: (provider == Provider::X).then(|| "6f1c2b9a8e7d4c3b2a1f0e9d8c7b6a59".into()),
        created_at: T0 - 30 * DAY,
        last_used_at: T0,
        current,
        removable: true,
    }
}

fn methods(x: MethodState) -> Vec<MethodRow> {
    vec![
        MethodRow { method: Provider::Google, state: MethodState::On },
        MethodRow { method: Provider::Github, state: MethodState::On },
        MethodRow { method: Provider::X, state: x },
        MethodRow { method: Provider::Bittensor, state: MethodState::On },
        MethodRow { method: Provider::Email, state: MethodState::On },
    ]
}

fn entries() -> Vec<(String, String)> {
    let mut e: Vec<(String, String)> = Vec::new();
    fn put<T: Serialize + DeserializeOwned + PartialEq + Debug>(e: &mut Vec<(String, String)>, name: &str, v: T) {
        let mut json = serde_json::to_string_pretty(&v).unwrap();
        json.push('\n');
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v, "{name}");
        e.push((format!("account/{name}.json"), json));
    }
    // Views.
    put(
        &mut e,
        "view-signin",
        View::Signin(SigninView { signed_in: false, methods: methods(MethodState::Absent), step_up: None }),
    );
    put(
        &mut e,
        "view-signin-x-off",
        View::Signin(SigninView { signed_in: false, methods: methods(MethodState::Off), step_up: None }),
    );
    put(
        &mut e,
        "view-signin-step-up",
        View::Signin(SigninView {
            signed_in: true,
            methods: methods(MethodState::On),
            step_up: Some(vec![
                StepUpChoice {
                    identity_id: 1,
                    provider: Provider::Email,
                    label: "reader@host.example".into(),
                    state: MethodState::On,
                },
                StepUpChoice {
                    identity_id: 2,
                    provider: Provider::Github,
                    label: "reader-fixture".into(),
                    state: MethodState::On,
                },
            ]),
        }),
    );
    let mut ids = vec![
        identity(1, Provider::Email, "reader@host.example", false),
        identity(2, Provider::Github, "reader-fixture", true),
        identity(3, Provider::X, "@reader_fixture", false),
        identity(4, Provider::Bittensor, SS58, false),
    ];
    put(
        &mut e,
        "view-account",
        View::Account(AccountView {
            label: "reader-fixture".into(),
            signed_in_with: Provider::Github,
            identities: ids.clone(),
            connect: vec![MethodRow { method: Provider::Google, state: MethodState::On }],
            step_up_fresh: false,
            read_only: false,
            billing: BillingState::Grants,
        }),
    );
    ids.truncate(1);
    ids[0].removable = false;
    ids[0].current = true;
    put(
        &mut e,
        "view-account-one-method",
        View::Account(AccountView {
            label: "reader@host.example".into(),
            signed_in_with: Provider::Email,
            identities: ids,
            connect: methods(MethodState::Absent).into_iter().filter(|m| m.method != Provider::Email).collect(),
            step_up_fresh: true,
            read_only: true,
            billing: BillingState::Off,
        }),
    );
    put(
        &mut e,
        "view-link-confirm",
        View::LinkConfirm(LinkConfirmView {
            label: "reader-fixture".into(),
            pending: Some(PendingView {
                kind: PendingKind::Link,
                provider: Provider::X,
                label: Some("@reader_fixture".into()),
                merge_blocked: false,
            }),
        }),
    );
    put(
        &mut e,
        "view-link-confirm-merge",
        View::LinkConfirm(LinkConfirmView {
            label: "reader-fixture".into(),
            pending: Some(PendingView {
                kind: PendingKind::Merge,
                provider: Provider::Google,
                label: Some("reader.other@host.example".into()),
                merge_blocked: true,
            }),
        }),
    );
    put(
        &mut e,
        "view-link-confirm-nothing",
        View::LinkConfirm(LinkConfirmView { label: "reader-fixture".into(), pending: None }),
    );
    put(
        &mut e,
        "view-sessions",
        View::Sessions(SessionsView {
            label: "reader-fixture".into(),
            sessions: vec![
                SessionRow {
                    session_ref: "3b7e0c1d9a8f6e5d".into(),
                    created_at: T0 - 3 * DAY,
                    last_seen_day: "2026-11-20".into(),
                    ua_family: Some("Firefox on Linux".into()),
                    method: Provider::Github,
                    current: true,
                },
                SessionRow {
                    session_ref: "8c4f2a6b1e0d9c7a".into(),
                    created_at: T0 - 20 * DAY,
                    last_seen_day: "2026-11-12".into(),
                    ua_family: None,
                    method: Provider::Email,
                    current: false,
                },
            ],
        }),
    );
    let owned = |id: u64, slug: &str, b: u8, is_owner: bool, s: Summary| OwnedEntry {
        entry_id: id,
        slug: slug.into(),
        id_h: IdH([b; 32]),
        is_owner,
        summary: s,
    };
    let mut paid = summary(Kind::PrivatePaid, None, 3214);
    paid.strong_sections = Some(1);
    paid.total_sections = Some(3);
    paid.expires_at = Some(T0 + 30 * DAY);
    let detections = vec![
        owned(11, "7hq2kd9m4xw8rt3vn6pb", 1, true, summary(Kind::PrivateText, Some(Band::VeryStrong), 181)),
        owned(12, "c3jf8kq1zw5ty7hn2mdx", 2, true, paid),
        owned(13, "m5tq9w2xk8vr3yn7hd4c", 3, false, summary(Kind::PrivateText, Some(Band::Weak), 412)),
    ];
    put(
        &mut e,
        "view-detections",
        View::Detections(DetectionsView {
            label: "reader-fixture".into(),
            entries: detections.clone(),
            read_only: false,
        }),
    );
    put(
        &mut e,
        "view-detections-empty",
        View::Detections(DetectionsView { label: "reader-fixture".into(), entries: Vec::new(), read_only: false }),
    );
    put(
        &mut e,
        "view-billing",
        View::Billing(BillingView {
            label: "reader-fixture".into(),
            state: BillingState::Grants,
            credit_tokens: 18_400,
            plan: Some(PlanView {
                starts_at: T0 - 10 * DAY,
                ends_at: T0 + 20 * DAY,
                tokens: 200_000,
                tokens_used: 4_310,
                api_tokens: 40_960,
                api_tokens_used: 0,
            }),
            history: vec![
                LedgerRow { at: T0, kind: LedgerRowKind::Debit, tokens: 4_310, cash: false },
                LedgerRow { at: T0 - 10 * DAY, kind: LedgerRowKind::PlanGrant, tokens: 200_000, cash: false },
                LedgerRow { at: T0 - 12 * DAY, kind: LedgerRowKind::Grant, tokens: 20_000, cash: false },
            ],
        }),
    );
    put(
        &mut e,
        "view-billing-off",
        View::Billing(BillingView {
            label: "reader-fixture".into(),
            state: BillingState::Off,
            credit_tokens: 0,
            plan: None,
            history: Vec::new(),
        }),
    );
    put(
        &mut e,
        "view-delete",
        View::Delete(DeleteView {
            label: "reader-fixture".into(),
            credit_tokens: 18_400,
            has_plan: true,
            saved_pages: 3,
            owned_pages: 2,
            step_up_fresh: false,
            default_mode: PurgeMode::Delete,
        }),
    );
    put(&mut e, "view-request-account", ViewRequest { page: ViewPage::Account, step_up: false });
    put(&mut e, "view-request-signin-step-up", ViewRequest { page: ViewPage::Signin, step_up: true });
    // Session and me.
    // The session answer carries the label, method and BILLING state for the
    // website's own pages; only user_id, auth_time and flags go to detect.
    let session = SessionAnswer {
        user_id: user(),
        auth_time: T0 - 5 * 60_000,
        flags: 0,
        label: "reader-fixture".into(),
        method: Provider::Github,
        billing: BillingState::Grants,
    };
    put(&mut e, "session", session.clone());
    put(
        &mut e,
        "session-paid-preview",
        SessionAnswer { flags: 1, label: SS58.into(), method: Provider::Bittensor, ..session.clone() },
    );
    put(&mut e, "session-billing-off", SessionAnswer { billing: BillingState::Off, ..session });
    let me = MeAnswer {
        signed_in: true,
        label: Some("reader-fixture".into()),
        method: Some(Provider::Github),
        billing: BillingState::Grants,
        paid_preview: false,
    };
    put(&mut e, "me-signed-in", me.clone());
    put(&mut e, "me-paid-preview", MeAnswer { paid_preview: true, ..me.clone() });
    put(&mut e, "me-billing-off", MeAnswer { billing: BillingState::Off, ..me });
    put(
        &mut e,
        "me-signed-out",
        MeAnswer { signed_in: false, label: None, method: None, billing: BillingState::Grants, paid_preview: false },
    );
    // Saved links.
    put(
        &mut e,
        "owned-add-request",
        OwnedAddRequest {
            user_id: user(),
            slug: "7hq2kd9m4xw8rt3vn6pb".into(),
            id_h: IdH([1; 32]),
            summary: summary(Kind::PrivateText, Some(Band::VeryStrong), 181),
            is_owner: true,
        },
    );
    put(&mut e, "owned-add-answer", OwnedAddAnswer { entry_id: 11, added: true });
    put(&mut e, "owned-entry-request", OwnedEntryRequest { user_id: user(), entry_id: 11 });
    put(&mut e, "owned-entry", detections[0].clone());
    put(&mut e, "owned-prune-request", OwnedPruneRequest { user_id: user(), entry_ids: vec![12, 13] });
    put(&mut e, "owned-removed", OwnedRemoved { removed: 2 });
    // Billing.
    put(&mut e, "billing-peek-request", PeekRequest { user_id: user() });
    put(
        &mut e,
        "billing-peek-answer",
        PeekAnswer { plan_left: 195_690, api_left: 40_960, credit_left: 18_400, paid_preview: true },
    );
    put(
        &mut e,
        "billing-hold-request",
        HoldRequest { user_id: user(), tokens: 3_020, channel: Channel::PaidPage, idem: "q-4f8c2a91d7e3b605".into() },
    );
    put(
        &mut e,
        "billing-hold-answer",
        HoldAnswer {
            hold_id: "h-2c9e7a1f4b8d3065".into(),
            parts: vec![
                HoldPart { src: PartSource::Period, id: 7, tokens: 2_000, cash: false },
                HoldPart { src: PartSource::Payment, id: 3, tokens: 1_020, cash: false },
            ],
        },
    );
    put(
        &mut e,
        "billing-commit-request",
        CommitRequest {
            hold_id: "h-2c9e7a1f4b8d3065".into(),
            receipt_id: "r-9d1e4c7b2a8f3065".into(),
            package_pub: "e2536d12".into(),
        },
    );
    put(&mut e, "billing-release-request", ReleaseRequest { hold_id: "h-2c9e7a1f4b8d3065".into() });
    put(
        &mut e,
        "billing-hold-end",
        HoldEndAnswer { hold_id: "h-2c9e7a1f4b8d3065".into(), ended: HoldEnd::Committed, by_this_call: true },
    );
    put(
        &mut e,
        "billing-hold-end-earlier",
        HoldEndAnswer { hold_id: "h-2c9e7a1f4b8d3065".into(), ended: HoldEnd::Committed, by_this_call: false },
    );
    put(&mut e, "billing-expire-request", ExpireRequest { idem: "6f1e0c2a9b8d7e3f5a4c1b0e9d8f7a6b".into() });
    put(&mut e, "billing-expire-released", ExpireAnswer { ended: Some(HoldEnd::Released), by_this_call: true });
    put(&mut e, "billing-expire-none", ExpireAnswer { ended: None, by_this_call: false });
    // Sign-in APIs.
    put(
        &mut e,
        "email-start",
        EmailStartRequest {
            email: Some("Kate@host.example".into()),
            return_to: Some("/detect/".into()),
            intent: None,
            identity_id: None,
        },
    );
    put(
        &mut e,
        "email-start-step-up",
        EmailStartRequest {
            email: None,
            return_to: Some("/account/".into()),
            intent: Some(MailIntent::StepUp),
            identity_id: Some(1),
        },
    );
    put(&mut e, "email-token", EmailTokenRequest { token: "email-fixture".into() });
    put(
        &mut e,
        "email-peek",
        EmailPeekAnswer { intent: MailIntent::Signin, address: "reader@host.example".into(), provider: None },
    );
    put(
        &mut e,
        "email-peek-attach",
        EmailPeekAnswer {
            intent: MailIntent::AttachPending,
            address: "reader@host.example".into(),
            provider: Some(Provider::Google),
        },
    );
    // The confirm and verify answers, for every intent.
    put(&mut e, "next", NextAnswer { return_to: LINK_CONFIRM_PATH.into() });
    // Account forms (urlencoded on the wire; the same field names here).
    for a in LinkAction::ALL {
        put(&mut e, &format!("form-link-confirm-{}", a.as_str()), LinkConfirmForm { action: *a });
    }
    put(&mut e, "form-unlink", UnlinkForm { identity_id: 3 });
    put(&mut e, "form-sessions-revoke", SessionRevokeForm { session_ref: "8c4f2a6b1e0d9c7a".into() });
    put(&mut e, "form-delete", DeleteAccountForm { confirm: "delete".into(), mode: PurgeMode::Delete });
    put(&mut e, "form-delete-release", DeleteAccountForm { confirm: "delete".into(), mode: PurgeMode::Release });
    put(&mut e, "form-empty", EmptyForm {});
    put(&mut e, "form-entry", EntryForm { entry_id: 11 });
    put(
        &mut e,
        "wallet-challenge-request",
        WalletChallengeRequest {
            address: SS58.into(),
            mode: WalletMode::Extension,
            intent: Intent::Signin,
            return_to: Some("/d/k7m2xq9".into()),
        },
    );
    put(
        &mut e,
        "wallet-challenge-request-link",
        WalletChallengeRequest { address: SS58.into(), mode: WalletMode::Cli, intent: Intent::Link, return_to: None },
    );
    put(
        &mut e,
        "wallet-challenge-answer",
        WalletChallengeAnswer {
            challenge_id: "challenge-fixture".into(),
            message: format!(
                "site.example wants you to sign in with your Bittensor account:\n{SS58}\n\nSign in to SlopNinja. This is not a \
transaction and costs nothing.\n\nURI: https://site.example/signin/\nVersion: 1\nNonce: 4kq8mz2p7wx3rt9vn6bc1hj5dy\n\
Issued At: 2026-11-20T10:04:00.000Z\nExpiration Time: 2026-11-20T10:09:00.000Z"
            ),
        },
    );
    put(
        &mut e,
        "wallet-verify-request",
        WalletVerifyRequest { challenge_id: "challenge-fixture".into(), signature: format!("0x{}", "ab".repeat(64)) },
    );
    // Its answers: a sign-in lands on its stored return_to; a link on the
    // confirm page; a step-up started from the account page back there.
    put(&mut e, "wallet-verify-answer", NextAnswer { return_to: Intent::Signin.next_path("/d/k7m2xq9").into() });
    put(&mut e, "wallet-verify-answer-link", NextAnswer { return_to: Intent::Link.next_path("/d/k7m2xq9").into() });
    put(
        &mut e,
        "wallet-verify-answer-step-up",
        NextAnswer { return_to: Intent::StepUp.next_path(Intent::StepUp.default_return_to()).into() },
    );
    e
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../detect-page/tests/fixtures")
}

#[test]
fn account_fixtures_match() {
    let bless = std::env::var_os("SLOP_NINJA_BLESS").is_some();
    let mut seen = BTreeSet::new();
    for (path, json) in entries() {
        assert!(seen.insert(path.clone()), "duplicate {path}");
        let p = dir().join(&path);
        if bless {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, &json).unwrap();
        } else {
            let on_disk = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("missing {path}"));
            assert_eq!(on_disk, json, "{path} differs (SLOP_NINJA_BLESS=1 rewrites it)");
        }
    }
    for f in std::fs::read_dir(dir().join("account")).unwrap() {
        let name = format!("account/{}", f.unwrap().file_name().to_string_lossy());
        assert!(seen.contains(&name), "stray fixture {name}");
    }
}

#[test]
fn every_view_page_has_a_fixture() {
    let all = entries();
    for p in ViewPage::ALL {
        let tag = format!("\"page\": \"{}\"", p.as_str());
        assert!(all.iter().any(|(path, json)| path.contains("/view-") && json.contains(&tag)), "{}", p.as_str());
    }
}

#[test]
fn mail_draws_on_the_right_buckets() {
    let u = UserId([9; 16]);
    let link = mail_buckets(MailIntent::Link, "aa", Some(u)).unwrap();
    assert_eq!(link.iter().map(Bucket::name).collect::<Vec<_>>(), ["email_link:aa", &format!("link_mail:{}", u.hex())]);
    // A connect mail never skips its account's quota.
    assert_eq!(mail_buckets(MailIntent::Link, "aa", None), Err(NoRequester));
    for m in [MailIntent::Signin, MailIntent::StepUp, MailIntent::AttachPending, MailIntent::Recover] {
        for r in [Some(u), None] {
            let b = mail_buckets(m, "aa", r).unwrap();
            assert_eq!(b.iter().map(Bucket::name).collect::<Vec<_>>(), ["email:aa"], "{}", m.as_str());
        }
    }
    // No link mail ever draws on the sign-in bucket.
    assert!(link.iter().all(|b| b.kind != BucketKind::Email));
    let names: BTreeSet<_> = BucketKind::ALL.iter().map(|k| k.policy_name()).collect();
    assert_eq!(names.len(), BucketKind::ALL.len());
    assert_eq!(mail_subject_prefix("[staging] "), Ok("[staging] "));
    assert_eq!(mail_subject_prefix(""), Ok(""));
    assert!(mail_subject_prefix("x\r\nBcc: a@b.example").is_err());
    assert!(mail_subject_prefix(&"x".repeat(MAX_SUBJECT_PREFIX + 1)).is_err());
}

#[test]
fn session_answer_becomes_the_principal() {
    let s = SessionAnswer {
        user_id: UserId([0xab; 16]),
        auth_time: 5,
        flags: 1,
        label: "reader@host.example".into(),
        method: Provider::Email,
        billing: BillingState::Grants,
    };
    // Only user_id, auth_time and flags are forwarded: the label, method and
    // BILLING state stay with the website.
    let header = s.principal().encode();
    assert_eq!(header, "v1;abababababababababababababababab;5;1");
    assert!(!header.contains("reader") && !header.contains("email") && !header.contains("grants"));
    assert!(s.paid_preview());
    assert!(!SessionAnswer { flags: 0, ..s.clone() }.paid_preview());
    assert!(Provider::X.is_oauth() && !Provider::Bittensor.is_oauth());
    assert_eq!(Provider::Bittensor.switch(), "auth.bittensor");
}

#[test]
fn the_paid_offer_follows_the_page_and_the_reader() {
    use slop_ninja_account_wire::detect_page::page::{PaidOffer, PaidReader};
    let page = |name: &str| -> slop_ninja_account_wire::detect_page::Page {
        serde_json::from_str(&std::fs::read_to_string(dir().join(format!("pages/{name}.json"))).unwrap()).unwrap()
    };
    let closed = page("27-unstored");
    let open = page("39-unstored-paid-open");
    assert_eq!((closed.paid_open, open.paid_open), (Some(false), Some(true)));
    let me = |name: &str| -> MeAnswer {
        serde_json::from_str(&std::fs::read_to_string(dir().join(format!("account/{name}.json"))).unwrap()).unwrap()
    };
    let session = |name: &str| -> SessionAnswer {
        serde_json::from_str(&std::fs::read_to_string(dir().join(format!("account/{name}.json"))).unwrap()).unwrap()
    };
    // The page scripts' answer and the website's session answer agree.
    for (m, s) in [
        ("me-signed-in", "session"),
        ("me-paid-preview", "session-paid-preview"),
        ("me-billing-off", "session-billing-off"),
    ] {
        assert_eq!(me(m).paid_reader(), Some(session(s).paid_reader()), "{m}");
    }
    assert_eq!(me("me-signed-out").paid_reader(), None);
    // Closed to the public (DETECT_PAID off): only a paid_preview account
    // with BILLING on sees the button; a signed-out reader is not invited.
    let cases: [(&str, Option<PaidReader>, PaidOffer, PaidOffer); 4] = [
        ("signed out", me("me-signed-out").paid_reader(), PaidOffer::Nothing, PaidOffer::SignIn),
        ("signed in", me("me-signed-in").paid_reader(), PaidOffer::Nothing, PaidOffer::CheckAll),
        ("paid_preview", me("me-paid-preview").paid_reader(), PaidOffer::CheckAll, PaidOffer::CheckAll),
        ("BILLING off", me("me-billing-off").paid_reader(), PaidOffer::Nothing, PaidOffer::Nothing),
    ];
    for (who, reader, when_closed, when_open) in cases {
        assert_eq!(closed.paid_offer(reader), when_closed, "{who}, closed");
        assert_eq!(open.paid_offer(reader), when_open, "{who}, open");
    }
    // A preview account with BILLING off sees nothing either.
    let off_preview = PaidReader { billing: BillingState::Off, paid_preview: true };
    assert_eq!(closed.paid_offer(Some(off_preview)), PaidOffer::Nothing);
    // No other page offers a paid check, whoever reads it.
    let best = Some(PaidReader { billing: BillingState::Live, paid_preview: true });
    for name in ["01-public-very-strong", "15-secret-text", "20-secret-free-cut", "28-example", "29-x-post-short"] {
        let p = page(name);
        assert_eq!(
            (p.paid_open, p.paid_offer(best), p.paid_offer(None)),
            (None, PaidOffer::Nothing, PaidOffer::Nothing)
        );
    }
}

#[test]
fn every_flow_answers_where_it_goes_next() {
    // A link always lands on the confirm page; every other flow on its own
    // stored destination.
    assert_eq!(Intent::Link.next_path("/d/k7m2xq9"), LINK_CONFIRM_PATH);
    assert_eq!(MailIntent::Link.next_path("/d/k7m2xq9"), LINK_CONFIRM_PATH);
    for i in [Intent::Signin, Intent::StepUp] {
        assert_eq!(i.next_path("/d/k7m2xq9"), "/d/k7m2xq9", "{}", i.as_str());
    }
    for m in [MailIntent::Signin, MailIntent::StepUp, MailIntent::AttachPending, MailIntent::Recover] {
        assert_eq!(m.next_path("/detect/"), "/detect/", "{}", m.as_str());
    }
    // An absent or refused return_to: the account page when signed in, the
    // tool otherwise.
    assert_eq!(Intent::Signin.default_return_to(), SIGNED_OUT_RETURN);
    assert_eq!(Intent::Link.default_return_to(), SIGNED_IN_RETURN);
    assert_eq!(Intent::StepUp.default_return_to(), SIGNED_IN_RETURN);
    for m in MailIntent::ALL {
        let want =
            if matches!(m, MailIntent::Link | MailIntent::StepUp) { SIGNED_IN_RETURN } else { SIGNED_OUT_RETURN };
        assert_eq!(m.default_return_to(), want, "{}", m.as_str());
    }
    assert_eq!(
        (SIGNED_IN_RETURN, SIGNED_OUT_RETURN, LINK_CONFIRM_PATH),
        ("/account/", "/detect/", "/account/link/confirm")
    );
    // The wallet challenge's return_to is optional on the wire.
    let c: WalletChallengeRequest =
        serde_json::from_str(r#"{"address":"5F","mode":"extension","intent":"step_up"}"#).unwrap();
    assert_eq!(c.return_to, None);
}

#[test]
fn every_account_form_route_has_its_body() {
    use slop_ninja_account_wire::detect_page::RouteId;
    use slop_ninja_account_wire::detect_page::sn::Callee;
    // Every forwarded account POST outside the JSON APIs and webhooks is a
    // form, and has its body type.
    for r in RouteId::ALL {
        let s = r.spec();
        let form = s.callee == Callee::Account && s.method == "POST" && s.csrf && !s.path.starts_with("/api/");
        assert_eq!(form_fields(*r).is_some(), form, "{}", r.as_str());
    }
    // There is no separate merge route: a merge is the link-confirm form's
    // `merge` action.
    assert!(RouteId::ALL.iter().all(|r| !r.spec().path.contains("merge")));
    assert_eq!(form_fields(RouteId::LinkConfirm), Some(&["action"][..]));
    // Each type's FIELDS are exactly its serde keys.
    fn keys<T: Serialize>(v: &T) -> Vec<String> {
        match serde_json::to_value(v).unwrap() {
            serde_json::Value::Object(m) => m.keys().cloned().collect(),
            _ => panic!("not an object"),
        }
    }
    let sorted = |f: &[&str]| {
        let mut v: Vec<String> = f.iter().map(|s| (*s).to_owned()).collect();
        v.sort();
        v
    };
    assert_eq!(keys(&LinkConfirmForm { action: LinkAction::Connect }), sorted(LinkConfirmForm::FIELDS));
    assert_eq!(keys(&UnlinkForm { identity_id: 1 }), sorted(UnlinkForm::FIELDS));
    assert_eq!(keys(&SessionRevokeForm { session_ref: "r".into() }), sorted(SessionRevokeForm::FIELDS));
    let delete = DeleteAccountForm { confirm: " Delete ".into(), mode: PurgeMode::Delete };
    assert_eq!(keys(&delete), sorted(DeleteAccountForm::FIELDS));
    assert_eq!(keys(&EmptyForm {}), sorted(EmptyForm::FIELDS));
    assert_eq!(keys(&EntryForm { entry_id: 1 }), sorted(EntryForm::FIELDS));
    // The typed word, and nothing else.
    assert!(delete.confirmed());
    for bad in ["", "delete it", "yes", "deleted"] {
        assert!(!DeleteAccountForm { confirm: bad.into(), mode: PurgeMode::Delete }.confirmed(), "{bad}");
    }
    // A forged body naming an identity is refused, not ignored.
    assert!(serde_json::from_str::<LinkConfirmForm>(r#"{"action":"connect","provider":"x","subject":"1"}"#).is_err());
    assert!(serde_json::from_str::<LinkConfirmForm>(r#"{"action":"attach"}"#).is_err());
    assert!(serde_json::from_str::<EmptyForm>(r#"{"user_id":"00"}"#).is_err());
}

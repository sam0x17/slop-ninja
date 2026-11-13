//! The separate budget for connect mail: after another account exhausts the
//! connect budget for an address, the holder's next sign-in mail is still
//! sent, and every send that draws on the sign-in bucket is one its holder
//! can use. The buckets and their keys are the account contract's; the
//! policy sizes them.

use std::collections::HashMap;

use slop_ninja_auth_core::account_wire::{BucketKind, NoRequester, UserId};
use slop_ninja_auth_core::detect_page::ae::account::{AUTH_LINK_CEILING, AUTH_MAIL_CEILING};
use slop_ninja_auth_core::email_addr::parse;
use slop_ninja_auth_core::mail::MailKind;
use slop_ninja_auth_core::policy::{AuthPolicy, BucketState, Take, ceiling_alert};
use slop_ninja_auth_core::providers::OAuthProvider;

fn user(n: u8) -> UserId {
    UserId([n; 16])
}

/// A stand-in for the quota object: every bucket a send draws on must have
/// a send left, or nothing is taken and nothing is sent. It keys its state
/// by the contract's bucket name (`<kind>:<key>`).
struct Quota {
    policy: AuthPolicy,
    buckets: HashMap<String, BucketState>,
    alerts: Vec<&'static str>,
}

impl Quota {
    fn new() -> Self {
        Self {
            policy: AuthPolicy::v1(),
            buckets: HashMap::new(),
            alerts: Vec::new(),
        }
    }

    fn send(
        &mut self,
        kind: MailKind,
        addr_h: &[u8; 32],
        requester: Option<UserId>,
        now_ms: u64,
    ) -> bool {
        let buckets = kind
            .buckets(addr_h, requester)
            .expect("a requester for connect mail");
        let mut next = Vec::new();
        let mut alerts = Vec::new();
        for b in &buckets {
            let size = self
                .policy
                .bucket(b.kind)
                .expect("auth-policy sizes every mail bucket");
            let name = b.name();
            match size.take(self.buckets.get(&name).copied(), now_ms) {
                Take::Taken { state, emptied } => {
                    next.push((name, state));
                    if emptied {
                        alerts.extend(ceiling_alert(b.kind));
                    }
                }
                Take::Empty { .. } => return false,
            }
        }
        self.buckets.extend(next);
        self.alerts.extend(alerts);
        true
    }
}

#[test]
fn connect_mail_never_spends_the_sign_in_budget() {
    let addr_h = parse("holder@host.example")
        .unwrap()
        .norm
        .keyed_hash(b"k_addr");
    let mut q = Quota::new();
    let t = 1_794_547_800_000;
    // Another account asks to connect the holder's address until the
    // per-address connect bucket is empty (three a day).
    let mut connects = 0;
    while q.send(MailKind::Link, &addr_h, Some(user(2)), t) {
        connects += 1;
    }
    assert_eq!(connects, 3);
    // The send that emptied it raised the connect ceiling alert, once.
    assert_eq!(q.alerts, [AUTH_LINK_CEILING]);
    // A third account is refused too: the per-address connect budget is
    // shared by every requester, so no address gets more than three a day.
    assert!(!q.send(MailKind::Link, &addr_h, Some(user(3)), t));
    // The holder's sign-in, step-up and attach mails are all still sent,
    // up to the sign-in ceiling of ten.
    for i in 0..10 {
        let kind = match i % 3 {
            0 => MailKind::Signin,
            1 => MailKind::StepUp,
            _ => MailKind::AttachPending {
                provider: OAuthProvider::Google,
            },
        };
        assert!(
            q.send(kind, &addr_h, None, t + i),
            "sign-in mail {i} stopped"
        );
    }
    assert!(!q.send(MailKind::Signin, &addr_h, None, t + 11));
    assert_eq!(q.alerts, [AUTH_LINK_CEILING, AUTH_MAIL_CEILING]);
}

#[test]
fn per_account_connect_quota() {
    let mut q = Quota::new();
    let t = 1_794_547_800_000;
    // One account may send connect mail to five distinct addresses a day.
    for n in 0..5u8 {
        assert!(q.send(MailKind::Link, &[n; 32], Some(user(7)), t));
    }
    assert!(!q.send(MailKind::Link, &[9; 32], Some(user(7)), t));
    // Another account is unaffected.
    assert!(q.send(MailKind::Link, &[9; 32], Some(user(8)), t));
    // An account's own quota raises no alert.
    assert!(q.alerts.is_empty());
}

#[test]
fn bucket_membership() {
    let addr_h = [0xab; 32];
    let hex_addr = "ab".repeat(32);
    let link = MailKind::Link.buckets(&addr_h, Some(user(1))).unwrap();
    assert_eq!(
        link.iter().map(|b| (b.kind, b.name())).collect::<Vec<_>>(),
        [
            (BucketKind::EmailLink, format!("email_link:{hex_addr}")),
            (
                BucketKind::LinkMail,
                format!("link_mail:{}", "01".repeat(16))
            ),
        ]
    );
    // A connect mail never goes out without the account that asked for it.
    assert_eq!(MailKind::Link.buckets(&addr_h, None), Err(NoRequester));
    for kind in [
        MailKind::Signin,
        MailKind::StepUp,
        MailKind::AttachPending {
            provider: OAuthProvider::X,
        },
        MailKind::Recover,
    ] {
        for requester in [None, Some(user(1))] {
            let b = kind.buckets(&addr_h, requester).unwrap();
            assert_eq!(
                b.iter().map(|b| (b.kind, b.name())).collect::<Vec<_>>(),
                [(BucketKind::Email, format!("email:{hex_addr}"))],
                "{kind:?}"
            );
        }
    }
}

#[test]
fn kinds_round_trip_through_stored_intents() {
    use slop_ninja_auth_core::account_wire::MailIntent;
    for kind in [
        MailKind::Signin,
        MailKind::Link,
        MailKind::StepUp,
        MailKind::AttachPending {
            provider: OAuthProvider::GitHub,
        },
        MailKind::Recover,
    ] {
        let provider = match kind {
            MailKind::AttachPending { provider } => Some(provider),
            _ => None,
        };
        assert_eq!(MailKind::of(kind.intent(), provider), Some(kind));
    }
    // An attach-pending mail names its provider.
    assert_eq!(MailKind::of(MailIntent::AttachPending, None), None);
    assert_eq!(MailIntent::ALL.len(), 5);
}

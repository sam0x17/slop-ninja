//! The separate budget for connect mail: after another account exhausts the
//! connect budget for an address, the holder's next sign-in mail is still
//! sent, and every send that draws on the sign-in bucket is one its holder
//! can use.

use std::collections::HashMap;

use slop_ninja_auth_core::email_addr::parse;
use slop_ninja_auth_core::mail::MailKind;
use slop_ninja_auth_core::policy::{
    AuthPolicy, BucketKind, BucketState, Take, email_key, email_link_key, link_mail_key,
};
use slop_ninja_auth_core::providers::Provider;

/// A stand-in for the quota object: every bucket a send draws on must have
/// a send left, or nothing is taken and nothing is sent.
struct Quota {
    policy: AuthPolicy,
    buckets: HashMap<String, BucketState>,
}

impl Quota {
    fn new() -> Self {
        Self {
            policy: AuthPolicy::v1(),
            buckets: HashMap::new(),
        }
    }

    fn send(&mut self, kind: MailKind, addr_h: &[u8; 32], user_id: u64, now_ms: u64) -> bool {
        let keys: Vec<(String, BucketKind)> = kind
            .buckets()
            .iter()
            .map(|b| {
                let key = match b {
                    BucketKind::Email => email_key(addr_h),
                    BucketKind::EmailLink => email_link_key(addr_h),
                    BucketKind::LinkMail => link_mail_key(user_id),
                };
                (key, *b)
            })
            .collect();
        let mut next = Vec::new();
        for (key, b) in &keys {
            match b
                .bucket(&self.policy)
                .take(self.buckets.get(key).copied(), now_ms)
            {
                Take::Taken { state, .. } => next.push((key.clone(), state)),
                Take::Empty { .. } => return false,
            }
        }
        self.buckets.extend(next);
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
    // Another account (user 2) asks to connect the holder's address until
    // the per-address connect bucket is empty (three a day).
    let mut connects = 0;
    while q.send(MailKind::Link, &addr_h, 2, t) {
        connects += 1;
    }
    assert_eq!(connects, 3);
    // A third account is refused too: the per-address connect budget is
    // shared by every requester, so no address gets more than three a day.
    assert!(!q.send(MailKind::Link, &addr_h, 3, t));
    // The holder's sign-in, step-up and attach mails are all still sent,
    // up to the sign-in ceiling of ten.
    for i in 0..10 {
        let kind = match i % 3 {
            0 => MailKind::Signin,
            1 => MailKind::StepUp,
            _ => MailKind::AttachPending {
                provider: Provider::Google,
            },
        };
        assert!(q.send(kind, &addr_h, 0, t + i), "sign-in mail {i} stopped");
    }
    assert!(!q.send(MailKind::Signin, &addr_h, 0, t + 11));
}

#[test]
fn per_account_connect_quota() {
    let mut q = Quota::new();
    let t = 1_794_547_800_000;
    // One account may send connect mail to five distinct addresses a day.
    for n in 0..5u8 {
        assert!(q.send(MailKind::Link, &[n; 32], 7, t));
    }
    assert!(!q.send(MailKind::Link, &[9; 32], 7, t));
    // Another account is unaffected.
    assert!(q.send(MailKind::Link, &[9; 32], 8, t));
}

#[test]
fn bucket_membership() {
    assert_eq!(
        MailKind::Link.buckets(),
        &[BucketKind::EmailLink, BucketKind::LinkMail]
    );
    for kind in [
        MailKind::Signin,
        MailKind::StepUp,
        MailKind::AttachPending {
            provider: Provider::X,
        },
        MailKind::Recover,
    ] {
        assert_eq!(kind.buckets(), &[BucketKind::Email]);
    }
}

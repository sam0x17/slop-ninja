//! The capturing mailer (feature `dev`): the captured recipient equals the
//! identity's subject byte for byte, each kind has its own copy, and failure
//! injection works.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use proptest::prelude::*;
use slop_ninja_auth_core::Origin;
use slop_ninja_auth_core::email_addr::parse;
use slop_ninja_auth_core::mail::capture::Capture;
use slop_ninja_auth_core::mail::{self, MailConfig, MailKind, Mailer, SendFailure};
use slop_ninja_auth_core::providers::OAuthProvider;
use slop_ninja_auth_core::token::Token;

/// Polls a future that completes without waiting (the capture never waits).
fn ready<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("the capture mailer never waits"),
    }
}

fn config(prefix: &str) -> MailConfig {
    MailConfig::new(
        "signin@mail.example",
        Origin::parse("https://site.example").unwrap(),
        prefix,
        false,
    )
    .unwrap()
}

#[test]
fn kinds_and_copy() {
    let capture = Capture::new();
    let to = parse("Kate@Host.example").unwrap().norm;
    let token = Token::from_random([9; 32]);
    let link = format!(
        "https://site.example/auth/email/confirm#t={}",
        token.encode()
    );
    let kinds = [
        (MailKind::Signin, "Your SlopNinja sign-in link"),
        (
            MailKind::Link,
            "Connect this address to a SlopNinja account",
        ),
        (MailKind::StepUp, "Confirm it's you on SlopNinja"),
        (
            MailKind::AttachPending {
                provider: OAuthProvider::GitHub,
            },
            "Connect your GitHub sign-in to SlopNinja",
        ),
    ];
    for (kind, subject) in kinds {
        ready(capture.send(&mail::compose(&config("[staging] "), kind, &to, &token, 15))).unwrap();
        let got = capture.take().pop().unwrap();
        assert_eq!(got.to, "kate@host.example");
        assert_eq!(got.from, "signin@mail.example");
        assert_eq!(got.subject, format!("[staging] {subject}"));
        assert!(got.text.contains(&link), "{subject}");
        assert!(got.html.contains(&link), "{subject}");
        assert!(got.text.contains("15 minutes"));
        // No other link, no image.
        assert_eq!(got.text.matches("https://").count(), 1);
        assert_eq!(got.html.matches("https://").count(), 2);
        assert!(!got.html.contains("<img"));
        let json = got.to_json();
        assert!(json.starts_with("{\"to\":\"kate@host.example\""));
    }
    // The sign-in copy exactly.
    let m = mail::compose(&config(""), MailKind::Signin, &to, &token, 15);
    assert_eq!(
        m.text,
        format!(
            "Sign in to SlopNinja:\n\n{link}\n\nThe link works once, for 15 minutes. If you didn't ask for it, ignore this email.\n"
        )
    );
    // A connect mail never reads like a sign-in mail.
    let l = mail::compose(&config(""), MailKind::Link, &to, &token, 15);
    assert!(!l.text.contains("Sign in to SlopNinja:"));
    assert!(l.text.contains("Nothing changes and nobody is signed in."));
}

#[test]
fn failure_injection() {
    let capture = Capture::new();
    let to = parse("kate@host.example").unwrap().norm;
    let m = mail::compose(
        &config(""),
        MailKind::Signin,
        &to,
        &Token::from_random([1; 32]),
        15,
    );
    capture.fail_next(SendFailure::Suppressed);
    assert_eq!(ready(capture.send(&m)), Err(SendFailure::Suppressed));
    assert!(capture.sent().is_empty());
    assert_eq!(ready(capture.send(&m)), Ok(()));
    assert_eq!(capture.sent().len(), 1);
}

proptest! {
    /// For every accepted input, the captured recipient equals the
    /// identity's subject, byte for byte.
    #[test]
    fn captured_recipient_is_the_subject(
        local in "[A-Za-z0-9._%+-]{1,16}",
        dom in "[A-Za-z0-9-]{1,10}\\.(example|EXAMPLE|b\u{00FC}cher)",
        ws in prop::sample::select(vec!["", " ", "\t", "\r\n"]),
    ) {
        let input = format!("{ws}{local}@{dom}{ws}");
        let Ok(addr) = parse(&input) else { return Ok(()); };
        let subject = addr.norm.as_str().as_bytes().to_vec();
        let capture = Capture::new();
        let m = mail::compose(&config(""), MailKind::Signin, &addr.norm, &Token::from_random([2; 32]), 15);
        prop_assert!(ready(capture.send(&m)).is_ok());
        let got = capture.take();
        prop_assert_eq!(got.len(), 1);
        prop_assert_eq!(got[0].to.as_bytes(), subject.as_slice());
    }
}

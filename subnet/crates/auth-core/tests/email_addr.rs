//! G-A1 email folding fixtures, and the property that a mail's recipient is
//! the identity's subject, byte for byte.

use proptest::prelude::*;
use slop_ninja_auth_core::email_addr::{EmailError, EmailNorm, parse};
use slop_ninja_auth_core::mail::{self, MailConfig, MailKind};
use slop_ninja_auth_core::origin::Origin;
use slop_ninja_auth_core::token::Token;

#[test]
fn non_ascii_local_parts_refused() {
    for addr in [
        "\u{212A}ate@example.com", // KELVIN SIGN, which Unicode case-folds to `k`
        "\u{FF4B}ate@example.com", // FULLWIDTH LATIN SMALL LETTER K
        "\u{0131}van@example.com", // LATIN SMALL LETTER DOTLESS I
        "ka\u{017F}e@example.com", // LATIN SMALL LETTER LONG S
        "kat\u{00E9}@example.com",
        "kate\u{200B}@example.com", // ZERO WIDTH SPACE
    ] {
        assert_eq!(parse(addr), Err(EmailError::Local), "{addr}");
    }
}

#[test]
fn ascii_case_is_the_one_fold() {
    let a = parse("KATE@Host.example").unwrap();
    let b = parse("kate@host.example").unwrap();
    assert_eq!(a.norm, b.norm);
    assert_eq!(a.norm.as_str(), "kate@host.example");
    assert_eq!(a.typed, "KATE@Host.example");
    // No dot or plus folding.
    assert_ne!(parse("k.ate@host.example").unwrap().norm, b.norm);
    assert_ne!(parse("kate+x@host.example").unwrap().norm, b.norm);
    // Trimming, not interior white space.
    assert_eq!(parse("  kate@host.example\n").unwrap().norm, b.norm);
}

#[test]
fn u_label_domain_becomes_a_label() {
    let a = parse("kate@b\u{00FC}cher.example").unwrap();
    assert_eq!(a.norm.as_str(), "kate@xn--bcher-kva.example");
    assert_eq!(a.domain(), "xn--bcher-kva.example");
    // UTS 46 maps the domain (full-width and upper case) before A-labels.
    assert_eq!(
        parse("kate@\u{FF25}XAMPLE.com").unwrap().norm.as_str(),
        "kate@example.com"
    );
    assert_eq!(parse("kate@xn--bcher-kva.example").unwrap().norm, a.norm);
}

#[test]
fn shape_rules() {
    for (addr, err) in [
        ("", EmailError::Length),
        ("   ", EmailError::Length),
        ("a@b@c.example", EmailError::At),
        ("kate.example", EmailError::At),
        ("ka\rte@host.example", EmailError::Control),
        ("ka\nte@host.example", EmailError::Control),
        ("kate@host.example\u{0}", EmailError::Control),
        ("\"kate\"@host.example", EmailError::Local),
        ("kate(x)@host.example", EmailError::Local),
        (".kate@host.example", EmailError::Local),
        ("ka..te@host.example", EmailError::Local),
        ("@host.example", EmailError::Local),
        ("kate@[127.0.0.1]", EmailError::Domain),
        ("kate@127.0.0.1", EmailError::Domain),
        ("kate@localhost", EmailError::Domain),
        ("kate@host.example.", EmailError::Domain),
        ("kate@-host.example", EmailError::Domain),
        ("kate@ho_st.example", EmailError::Domain),
        ("kate@", EmailError::Domain),
    ] {
        assert_eq!(parse(addr), Err(err), "{addr:?}");
    }
    let long_local = format!("{}@host.example", "a".repeat(65));
    assert_eq!(parse(&long_local), Err(EmailError::Local));
    let long = format!("{}@{}.example", "a".repeat(64), "b".repeat(190));
    assert_eq!(parse(&long), Err(EmailError::Length));
    assert!(parse("o'brien+tag!#$%&*/=?^_`{|}~-@host.example").is_ok());
}

#[test]
fn stored_subject_revalidates() {
    assert!(EmailNorm::from_stored("kate@host.example").is_some());
    assert!(EmailNorm::from_stored("Kate@host.example").is_none());
    assert!(EmailNorm::from_stored("kate@b\u{00FC}cher.example").is_none());
}

fn config() -> MailConfig {
    MailConfig::new(
        "signin@mail.example",
        Origin::parse("https://site.example").unwrap(),
        "",
        false,
    )
    .unwrap()
}

fn local_part() -> impl Strategy<Value = String> {
    "[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]{1,12}(\\.[A-Za-z0-9_+-]{1,8}){0,2}"
}

fn domain() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "host",
            "Example",
            "b\u{00FC}cher",
            "\u{4F8B}\u{3048}",
            "MAIL",
            "sub-domain",
            "xn--bcher-kva",
        ]),
        1..3,
    )
    .prop_map(|labels| format!("{}.example", labels.join(".")))
}

proptest! {
    /// For every accepted input, the composed mail's recipient is the
    /// identity's subject, byte for byte, for every mail kind.
    #[test]
    fn recipient_is_the_subject(
        local in local_part(),
        dom in domain(),
        pad in prop::sample::select(vec!["", " ", "\t", "\n "]),
    ) {
        let input = format!("{pad}{local}@{dom}{pad}");
        let Ok(addr) = parse(&input) else { return Ok(()); };
        let subject = addr.norm.as_str().to_owned();
        // The normal form is ASCII, lower case, and re-validates as stored.
        prop_assert!(subject.is_ascii());
        prop_assert_eq!(subject.to_ascii_lowercase(), subject.clone());
        prop_assert_eq!(EmailNorm::from_stored(&subject).map(|n| n.as_str().to_owned()), Some(subject.clone()));
        let token = Token::from_random([3; 32]);
        for kind in [
            MailKind::Signin,
            MailKind::Link,
            MailKind::StepUp,
            MailKind::AttachPending { provider: slop_ninja_auth_core::providers::Provider::GitHub },
        ] {
            let m = mail::compose(&config(), kind, &addr.norm, &token, 15);
            prop_assert_eq!(m.to.as_str().as_bytes(), subject.as_bytes());
            // The typed form never appears in a mail.
            if addr.typed != subject {
                prop_assert!(!m.text.contains(&addr.typed));
                prop_assert!(!m.html.contains(&addr.typed));
            }
        }
    }
}

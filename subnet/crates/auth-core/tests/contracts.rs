//! auth-core against the shared contracts: every refusal it can produce is
//! one of the contract's refusal codes, on the sign-in or request surface,
//! with its fixture; and the sign-in fixtures the account Worker and the
//! pages share pass through auth-core's checks as the contract says.

use std::path::PathBuf;

use slop_ninja_auth_core::account_wire::{
    EmailStartRequest, Intent, MailIntent, NextAnswer, View, WalletChallengeRequest, WalletMode,
    WalletVerifyRequest,
};
use slop_ninja_auth_core::csrf::{CONTENT_TYPE_REFUSAL, CsrfRefusal};
use slop_ninja_auth_core::detect_page::refusal::{ApiRefusal, Group};
use slop_ninja_auth_core::email_addr::{self, EmailError};
use slop_ninja_auth_core::oidc::ClaimError;
use slop_ninja_auth_core::policy::AuthPolicy;
use slop_ninja_auth_core::providers::{OAuthProvider, ProviderError};
use slop_ninja_auth_core::return_to::{self, ReturnTo};
use slop_ninja_auth_core::state::{Flow, ProviderDenial, Refusal};
use slop_ninja_auth_core::{Origin, RefusalCode};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../detect-page/tests/fixtures")
}

fn fixture(path: &str) -> String {
    let p = fixtures().join(path);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn site() -> Origin {
    // The fixtures are written for this placeholder origin.
    Origin::parse("https://site.example").unwrap()
}

/// Every code auth-core can answer with, each with where it came from.
fn produced() -> Vec<(String, RefusalCode)> {
    let mut out = Vec::new();
    let refusals = [
        Refusal::MalformedQuery,
        Refusal::MissingState,
        Refusal::MalformedState,
        Refusal::MissingCode,
        Refusal::Unknown,
        Refusal::MissingFlowCookie,
        Refusal::MalformedFlowCookie,
        Refusal::BindingMismatch,
        Refusal::ProviderMismatch,
        Refusal::Expired,
    ];
    for flow in [Flow::OAuth, Flow::EmailLink, Flow::WalletChallenge] {
        for r in refusals {
            out.push((format!("{r:?} in {flow:?}"), r.code(flow)));
        }
    }
    for d in [ProviderDenial::AccessDenied, ProviderDenial::Other] {
        out.push((format!("{d:?}"), d.code()));
    }
    for e in [
        ProviderError::TooLarge,
        ProviderError::Malformed,
        ProviderError::Refused,
        ProviderError::Unavailable,
        ProviderError::Upstream,
    ] {
        out.push((format!("{e:?}"), e.code()));
    }
    for e in [
        ClaimError::Malformed,
        ClaimError::Issuer,
        ClaimError::Audience,
        ClaimError::Expired,
        ClaimError::IssuedInFuture,
        ClaimError::Nonce,
        ClaimError::Subject,
    ] {
        out.push((format!("{e:?}"), e.code()));
    }
    for e in [
        EmailError::Length,
        EmailError::Control,
        EmailError::At,
        EmailError::Local,
        EmailError::Domain,
    ] {
        out.push((format!("{e:?}"), e.code()));
    }
    for e in [
        CsrfRefusal::Method,
        CsrfRefusal::FetchSite,
        CsrfRefusal::Origin,
    ] {
        out.push((format!("{e:?}"), e.code()));
    }
    out.push(("content type".to_owned(), CONTENT_TYPE_REFUSAL));
    #[cfg(feature = "wallet")]
    for e in [
        slop_ninja_auth_core::siws::SiwsError::Address,
        slop_ninja_auth_core::siws::SiwsError::SignatureFormat,
        slop_ninja_auth_core::siws::SiwsError::BadSignature,
    ] {
        out.push((format!("{e:?}"), e.code()));
    }
    out
}

#[test]
fn every_refusal_is_a_contract_code_with_its_fixture() {
    for (from, code) in produced() {
        assert!(
            matches!(code.group(), Group::Signin | Group::Request),
            "{from}: {code} is on the {:?} surface",
            code.group()
        );
        let parsed: ApiRefusal =
            serde_json::from_str(&fixture(&format!("refusals/{}.json", code.as_str()))).unwrap();
        assert_eq!(parsed.code, code, "{from}");
        assert_eq!(parsed, ApiRefusal::new(code), "{from}");
    }
}

#[test]
fn refusal_codes_by_flow() {
    use RefusalCode as C;
    // OAuth: anything wrong with `state` or the flow cookie is `state`.
    for r in [
        Refusal::Unknown,
        Refusal::BindingMismatch,
        Refusal::MissingFlowCookie,
        Refusal::ProviderMismatch,
        Refusal::MalformedState,
    ] {
        assert_eq!(r.code(Flow::OAuth), C::State, "{r:?}");
    }
    assert_eq!(Refusal::Expired.code(Flow::OAuth), C::Expired);
    // An email link: spent, unknown or expired reads as expired, like its
    // peek; the browser rule is link_wrong_browser.
    assert_eq!(Refusal::Unknown.code(Flow::EmailLink), C::Expired);
    assert_eq!(Refusal::Expired.code(Flow::EmailLink), C::Expired);
    for r in [
        Refusal::BindingMismatch,
        Refusal::MissingFlowCookie,
        Refusal::MalformedFlowCookie,
    ] {
        assert_eq!(r.code(Flow::EmailLink), C::LinkWrongBrowser, "{r:?}");
    }
    // A wallet challenge: reused or expired is expired; another browser's
    // flow cookie is `state`.
    assert_eq!(Refusal::Unknown.code(Flow::WalletChallenge), C::Expired);
    assert_eq!(
        Refusal::BindingMismatch.code(Flow::WalletChallenge),
        C::State
    );
    // Providers.
    assert_eq!(ProviderDenial::AccessDenied.code(), C::Denied);
    assert_eq!(ProviderError::Unavailable.code(), C::MethodOff);
    assert_eq!(ProviderError::Refused.code(), C::Provider);
    assert_eq!(ClaimError::Nonce.code(), C::Provider);
}

#[test]
fn email_start_fixtures() {
    let p = AuthPolicy::v1();
    let req: EmailStartRequest =
        serde_json::from_str(&fixture("account/email-start.json")).unwrap();
    let addr = email_addr::parse(req.email.as_deref().unwrap()).unwrap();
    // The one fold: ASCII lower case; the typed form is kept for its holder.
    assert_eq!(addr.norm.as_str(), "kate@host.example");
    assert_eq!(addr.typed, "Kate@host.example");
    let intent = req.intent.unwrap_or(MailIntent::Signin);
    let r = return_to::sanitize_or_default(
        req.return_to.as_deref(),
        &site(),
        &p,
        intent.is_signed_in(),
    );
    assert_eq!(r.as_str(), "/detect/");

    let step_up: EmailStartRequest =
        serde_json::from_str(&fixture("account/email-start-step-up.json")).unwrap();
    assert_eq!(step_up.intent, Some(MailIntent::StepUp));
    let r = return_to::sanitize_or_default(step_up.return_to.as_deref(), &site(), &p, true);
    assert_eq!(r.as_str(), "/account/");
}

#[test]
fn defaults_are_the_contract_defaults() {
    let p = AuthPolicy::v1();
    for &i in Intent::ALL {
        assert_eq!(
            return_to::sanitize_or_default(None, &site(), &p, i.is_signed_in()).as_str(),
            i.default_return_to(),
            "{i:?}"
        );
    }
    for &i in MailIntent::ALL {
        assert_eq!(
            return_to::sanitize_or_default(None, &site(), &p, i.is_signed_in()).as_str(),
            i.default_return_to(),
            "{i:?}"
        );
    }
}

#[test]
fn stored_return_paths_in_the_answers_pass_unchanged() {
    let p = AuthPolicy::v1();
    for path in [
        "account/next.json",
        "account/wallet-verify-answer.json",
        "account/wallet-verify-answer-link.json",
        "account/wallet-verify-answer-step-up.json",
    ] {
        let next: NextAnswer = serde_json::from_str(&fixture(path)).unwrap();
        assert_eq!(
            ReturnTo::from_stored(&next.return_to, &site(), &p, true).as_str(),
            next.return_to,
            "{path}"
        );
    }
    let link: NextAnswer = serde_json::from_str(&fixture("account/next.json")).unwrap();
    assert_eq!(
        link.return_to,
        Intent::Link.next_path(return_to::SIGNED_IN_DEFAULT)
    );
}

#[test]
fn wallet_request_fixtures() {
    let p = AuthPolicy::v1();
    let req: WalletChallengeRequest =
        serde_json::from_str(&fixture("account/wallet-challenge-request.json")).unwrap();
    assert_eq!(req.mode, WalletMode::Extension);
    assert_eq!(
        return_to::sanitize_or_default(
            req.return_to.as_deref(),
            &site(),
            &p,
            req.intent.is_signed_in()
        )
        .as_str(),
        "/d/k7m2xq9"
    );
    let link: WalletChallengeRequest =
        serde_json::from_str(&fixture("account/wallet-challenge-request-link.json")).unwrap();
    assert_eq!(link.mode, WalletMode::Cli);
    assert_eq!(
        return_to::sanitize_or_default(
            link.return_to.as_deref(),
            &site(),
            &p,
            link.intent.is_signed_in()
        )
        .as_str(),
        return_to::SIGNED_IN_DEFAULT
    );
    #[cfg(feature = "wallet")]
    {
        use slop_ninja_auth_core::siws::{self, SiwsError};
        // The fixture address is a placeholder that is not SS58.
        assert_eq!(
            siws::decode_address(&req.address).map_err(SiwsError::code),
            Err(RefusalCode::WalletType)
        );
        // The fixture signature has the accepted form and verifies over
        // nothing.
        let v: WalletVerifyRequest =
            serde_json::from_str(&fixture("account/wallet-verify-request.json")).unwrap();
        let sig = siws::parse_signature(&v.signature).unwrap();
        assert_eq!(
            siws::verify(b"any message", &sig, &[7; 32]).map_err(SiwsError::code),
            Err(RefusalCode::Signature)
        );
    }
    #[cfg(not(feature = "wallet"))]
    {
        let _: WalletVerifyRequest =
            serde_json::from_str(&fixture("account/wallet-verify-request.json")).unwrap();
    }
}

/// The challenge fixture's message has exactly the layout `siws::message`
/// builds, apart from its placeholder address and nonce.
#[cfg(feature = "wallet")]
#[test]
fn wallet_challenge_fixture_layout() {
    use slop_ninja_auth_core::account_wire::WalletChallengeAnswer;
    use slop_ninja_auth_core::siws::{self, Challenge, Nonce};

    let answer: WalletChallengeAnswer =
        serde_json::from_str(&fixture("account/wallet-challenge-answer.json")).unwrap();
    // 2026-11-20T10:04:00.000Z, and five minutes later.
    let issued = 1_795_169_040_000;
    let expires = issued + AuthPolicy::v1().wallet_challenge_ttl_s.value * 1000;
    let site = site();
    let built = siws::message(
        WalletMode::Extension,
        &Challenge {
            site: &site,
            public_key: &[1; 32],
            nonce: &Nonce::from_random([2; 16]),
            issued_at_ms: issued,
            expires_at_ms: expires,
        },
    );
    let ours: Vec<&str> = built.split('\n').collect();
    let theirs: Vec<&str> = answer.message.split('\n').collect();
    assert_eq!(ours.len(), theirs.len());
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        match i {
            // The address line.
            1 => assert_eq!(a.len(), 48),
            // The nonce line: the label, then 26 characters.
            7 => {
                assert_eq!(a.split_once(": ").unwrap().0, b.split_once(": ").unwrap().0);
                assert_eq!(a.len(), b.len());
            }
            _ => assert_eq!(a, b, "line {i}"),
        }
    }
}

#[test]
fn sign_in_view_lists_the_oauth_providers_in_button_order() {
    let view: View = serde_json::from_str(&fixture("account/view-signin.json")).unwrap();
    let View::Signin(v) = view else {
        panic!("not the sign-in view")
    };
    let oauth: Vec<OAuthProvider> = v
        .methods
        .iter()
        .filter_map(|m| OAuthProvider::of(m.method))
        .collect();
    assert_eq!(oauth, OAuthProvider::ALL);
}

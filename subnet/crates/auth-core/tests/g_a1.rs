//! Gate G-A1, the auth-core rows: `state`, the flow cookie, PKCE, the ID
//! token claims, email token reuse and the cookie strings. The wallet rows
//! are in `tests/siws.rs`, `return_to` in `tests/return_to.rs`, email
//! folding in `tests/email_addr.rs` and the CSRF matrix in `tests/csrf.rs`.

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use slop_ninja_auth_core::oidc::{self, ClaimError, Expect};
use slop_ninja_auth_core::pkce::{self, Verifier};
use slop_ninja_auth_core::providers::{GOOGLE_ISSUERS, OAuthProvider};
use slop_ninja_auth_core::state::{
    CallbackQuery, FlowFacts, Refusal, check_email_token, check_flow,
};
use slop_ninja_auth_core::token::Token;
use slop_ninja_auth_core::{cookie, policy::AuthPolicy};

const NOW_MS: u64 = 1_794_547_800_000;

fn tok(n: u8) -> Token {
    Token::from_random([n; 32])
}

/// A stand-in for `DELETE … WHERE key = ? RETURNING …`: each key is taken
/// at most once.
struct TakeOnce<V>(HashMap<[u8; 32], V>);

impl<V: Clone> TakeOnce<V> {
    fn new() -> Self {
        Self(HashMap::new())
    }
    fn insert(&mut self, key: [u8; 32], v: V) {
        self.0.insert(key, v);
    }
    fn take(&mut self, key: &[u8; 32]) -> Option<V> {
        self.0.remove(key)
    }
}

struct Started {
    state: Token,
    binding: Token,
}

fn start(store: &mut TakeOnce<FlowFacts>, provider: OAuthProvider, n: u8) -> Started {
    let state = tok(n);
    let binding = tok(n.wrapping_add(100));
    store.insert(
        state.hash(),
        FlowFacts {
            provider,
            binding_h: binding.hash(),
            expires_at_ms: NOW_MS + AuthPolicy::v1().flow_ttl_s.value * 1000,
        },
    );
    Started { state, binding }
}

fn callback(
    store: &mut TakeOnce<FlowFacts>,
    query: &str,
    cookie_header: Option<&str>,
    provider: OAuthProvider,
    now_ms: u64,
) -> Result<String, Refusal> {
    let q = CallbackQuery::parse(query)?;
    let taken = store.take(&q.state.hash());
    check_flow(
        taken.as_ref(),
        cookie::get(cookie_header, cookie::FLOW),
        provider,
        now_ms,
    )?;
    q.outcome.map_err(|_| Refusal::MissingCode)
}

fn flow_cookie(t: &Token) -> String {
    format!("{}={}", cookie::FLOW, t.encode())
}

#[test]
fn state_and_flow_cookie() {
    let mut store = TakeOnce::new();
    let s = start(&mut store, OAuthProvider::Google, 1);
    let query = format!("code=c0de&state={}", s.state.encode());
    let header = flow_cookie(&s.binding);

    // The good path, once.
    assert_eq!(
        callback(
            &mut store,
            &query,
            Some(&header),
            OAuthProvider::Google,
            NOW_MS
        ),
        Ok("c0de".to_owned())
    );
    // Replayed state: the row is gone.
    assert_eq!(
        callback(
            &mut store,
            &query,
            Some(&header),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::Unknown)
    );

    // A state never issued.
    let other = format!("code=c0de&state={}", tok(9).encode());
    assert_eq!(
        callback(
            &mut store,
            &other,
            Some(&header),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::Unknown)
    );
    // Missing and malformed state.
    assert_eq!(
        callback(
            &mut store,
            "code=c0de",
            Some(&header),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::MissingState)
    );
    assert_eq!(
        callback(
            &mut store,
            "code=c0de&state=abc",
            Some(&header),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::MalformedState)
    );

    // Missing flow cookie; the state is spent anyway.
    let s = start(&mut store, OAuthProvider::Google, 2);
    let q = format!("code=c0de&state={}", s.state.encode());
    assert_eq!(
        callback(&mut store, &q, None, OAuthProvider::Google, NOW_MS),
        Err(Refusal::MissingFlowCookie)
    );
    assert_eq!(
        callback(
            &mut store,
            &q,
            Some(&flow_cookie(&s.binding)),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::Unknown)
    );

    // A different flow cookie (an attacker's own flow, or another browser).
    let s = start(&mut store, OAuthProvider::Google, 3);
    let q = format!("code=c0de&state={}", s.state.encode());
    assert_eq!(
        callback(
            &mut store,
            &q,
            Some(&flow_cookie(&tok(77))),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::BindingMismatch)
    );

    // A malformed flow cookie, and two flow cookies at once.
    let s = start(&mut store, OAuthProvider::Google, 4);
    let q = format!("code=c0de&state={}", s.state.encode());
    assert_eq!(
        callback(
            &mut store,
            &q,
            Some("__Host-sn_flow=zz"),
            OAuthProvider::Google,
            NOW_MS
        ),
        Err(Refusal::MalformedFlowCookie)
    );
    let s = start(&mut store, OAuthProvider::Google, 5);
    let q = format!("code=c0de&state={}", s.state.encode());
    let two = format!("{}; {}", flow_cookie(&s.binding), flow_cookie(&tok(6)));
    assert_eq!(
        callback(&mut store, &q, Some(&two), OAuthProvider::Google, NOW_MS),
        Err(Refusal::MissingFlowCookie)
    );

    // A Google state arriving at the GitHub callback (mix-up).
    let s = start(&mut store, OAuthProvider::Google, 7);
    let q = format!("code=c0de&state={}", s.state.encode());
    assert_eq!(
        callback(
            &mut store,
            &q,
            Some(&flow_cookie(&s.binding)),
            OAuthProvider::GitHub,
            NOW_MS
        ),
        Err(Refusal::ProviderMismatch)
    );

    // Expired.
    let s = start(&mut store, OAuthProvider::X, 8);
    let q = format!("code=c0de&state={}", s.state.encode());
    assert_eq!(
        callback(
            &mut store,
            &q,
            Some(&flow_cookie(&s.binding)),
            OAuthProvider::X,
            NOW_MS + 600_000
        ),
        Err(Refusal::Expired)
    );
}

#[test]
fn pkce_mismatch() {
    let issued = Verifier::from_random([11; 32]);
    let challenge = issued.challenge();
    assert!(pkce::verify(issued.as_str(), &challenge));
    // Another flow's verifier, or the challenge sent back as `plain`.
    assert!(!pkce::verify(
        Verifier::from_random([12; 32]).as_str(),
        &challenge
    ));
    assert!(!pkce::verify(&challenge, &challenge));
}

fn id_token(claims: &str) -> String {
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims.as_bytes()),
        URL_SAFE_NO_PAD.encode(b"sig")
    )
}

const NOW_S: u64 = NOW_MS / 1000;

fn expect<'a>(nonce: &'a str) -> Expect<'a> {
    Expect {
        issuers: &GOOGLE_ISSUERS,
        client_id: "client-1.apps.example",
        nonce,
        now_s: NOW_S,
    }
}

fn claims(iss: &str, aud: &str, exp: u64, nonce: &str) -> String {
    format!(
        r#"{{"iss":"{iss}","aud":{aud},"exp":{exp},"iat":{},"nonce":"{nonce}","sub":"1045","email":"Kate@Example.com","email_verified":true}}"#,
        NOW_S - 5
    )
}

#[test]
fn id_token_claims() {
    let nonce = tok(21).encode();
    let aud = r#""client-1.apps.example""#;
    // Both forms of `iss`.
    for iss in GOOGLE_ISSUERS {
        let id = oidc::check(
            &id_token(&claims(iss, aud, NOW_S + 60, &nonce)),
            &expect(&nonce),
        )
        .unwrap();
        assert_eq!(id.sub, "1045");
        assert_eq!(id.verified_email.unwrap().norm.as_str(), "kate@example.com");
    }
    let cases = [
        (
            claims(
                "https://accounts.google.com.evil.example",
                aud,
                NOW_S + 60,
                &nonce,
            ),
            ClaimError::Issuer,
        ),
        (
            claims("http://accounts.google.com", aud, NOW_S + 60, &nonce),
            ClaimError::Issuer,
        ),
        (
            claims(
                "https://accounts.google.com",
                r#""someone-else""#,
                NOW_S + 60,
                &nonce,
            ),
            ClaimError::Audience,
        ),
        (
            claims(
                "https://accounts.google.com",
                r#"["someone-else"]"#,
                NOW_S + 60,
                &nonce,
            ),
            ClaimError::Audience,
        ),
        (
            claims(
                "https://accounts.google.com",
                r#"["client-1.apps.example","x"]"#,
                NOW_S + 60,
                &nonce,
            ),
            ClaimError::Audience,
        ),
        (
            claims("https://accounts.google.com", aud, NOW_S, &nonce),
            ClaimError::Expired,
        ),
        (
            claims("https://accounts.google.com", aud, NOW_S - 1, &nonce),
            ClaimError::Expired,
        ),
        (
            claims(
                "https://accounts.google.com",
                aud,
                NOW_S + 60,
                &tok(22).encode(),
            ),
            ClaimError::Nonce,
        ),
    ];
    for (c, want) in cases {
        assert_eq!(
            oidc::check(&id_token(&c), &expect(&nonce)),
            Err(want),
            "{c}"
        );
    }
    // Missing claims.
    for (c, want) in [
        (
            r#"{"aud":"client-1.apps.example","exp":9999999999,"nonce":"n","sub":"1"}"#,
            ClaimError::Issuer,
        ),
        (
            r#"{"iss":"accounts.google.com","exp":9999999999,"nonce":"n","sub":"1"}"#,
            ClaimError::Audience,
        ),
        (
            r#"{"iss":"accounts.google.com","aud":"client-1.apps.example","nonce":"n","sub":"1"}"#,
            ClaimError::Expired,
        ),
        (
            r#"{"iss":"accounts.google.com","aud":"client-1.apps.example","exp":9999999999,"sub":"1"}"#,
            ClaimError::Nonce,
        ),
        (
            r#"{"iss":"accounts.google.com","aud":"client-1.apps.example","exp":9999999999,"nonce":"n"}"#,
            ClaimError::Subject,
        ),
    ] {
        assert_eq!(oidc::check(&id_token(c), &expect("n")), Err(want), "{c}");
    }
    // A multi-audience token with the right `azp` passes.
    let multi = format!(
        r#"{{"iss":"accounts.google.com","aud":["client-1.apps.example","x"],"azp":"client-1.apps.example","exp":{},"nonce":"n","sub":"7"}}"#,
        NOW_S + 60
    );
    assert!(oidc::check(&id_token(&multi), &expect("n")).is_ok());
    // An unverified email is not a verified email.
    let unverified = format!(
        r#"{{"iss":"accounts.google.com","aud":"client-1.apps.example","exp":{},"nonce":"n","sub":"7","email":"a@b.example","email_verified":false}}"#,
        NOW_S + 60
    );
    assert_eq!(
        oidc::check(&id_token(&unverified), &expect("n"))
            .unwrap()
            .verified_email,
        None
    );
    // Shape: two segments, four segments, bad base64, a non-JSON payload,
    // a duplicated claim.
    for bad in [
        "a.b".to_owned(),
        "a.b.c.d".to_owned(),
        "a.!!.c".to_owned(),
        format!("a.{}.c", URL_SAFE_NO_PAD.encode(b"not json")),
        id_token(r#"{"iss":"accounts.google.com","iss":"accounts.google.com"}"#),
    ] {
        assert_eq!(
            oidc::check(&bad, &expect("n")),
            Err(ClaimError::Malformed),
            "{bad}"
        );
    }
}

#[test]
fn email_token_reuse_and_expiry() {
    let mut store: TakeOnce<u64> = TakeOnce::new();
    let t = tok(31);
    let ttl = AuthPolicy::v1().email_link_ttl_s.value * 1000;
    store.insert(t.hash(), NOW_MS + ttl);
    // Confirm parses the posted token and takes its row.
    let posted = Token::parse(&t.encode()).unwrap();
    assert_eq!(
        check_email_token(store.take(&posted.hash()), NOW_MS + 1),
        Ok(())
    );
    // Reuse finds nothing.
    assert_eq!(
        check_email_token(store.take(&posted.hash()), NOW_MS + 2),
        Err(Refusal::Unknown)
    );
    // Expired.
    let t2 = tok(32);
    store.insert(t2.hash(), NOW_MS + ttl);
    assert_eq!(
        check_email_token(store.take(&t2.hash()), NOW_MS + ttl),
        Err(Refusal::Expired)
    );
}

#[test]
fn cookie_strings() {
    let t = tok(41);
    let policy = AuthPolicy::v1();
    assert_eq!(
        cookie::set(cookie::SESSION, &t, policy.session_idle_s.value),
        format!(
            "__Host-sn_session={}; Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Lax",
            t.encode()
        )
    );
    assert_eq!(
        cookie::set(cookie::FLOW, &t, policy.flow_ttl_s.value),
        format!(
            "__Host-sn_flow={}; Max-Age=600; Path=/; Secure; HttpOnly; SameSite=Lax",
            t.encode()
        )
    );
    assert_eq!(
        cookie::clear(cookie::FLOW),
        "__Host-sn_flow=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax"
    );
    for s in [
        cookie::set(cookie::SESSION, &t, 1),
        cookie::clear(cookie::SESSION),
    ] {
        // `__Host-` cookies: Secure, Path=/ and no Domain.
        assert!(s.contains("; Secure") && s.contains("; Path=/") && !s.contains("Domain"));
    }
    let header = format!("other=1; {}={}", cookie::SESSION, t.encode());
    assert_eq!(cookie::token(Some(&header), cookie::SESSION), Some(t));
    assert_eq!(
        cookie::token(Some("__Host-sn_session=short"), cookie::SESSION),
        None
    );
}

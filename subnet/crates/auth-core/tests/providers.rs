//! Provider tables, request builders and the bounded response parsers.

use slop_ninja_auth_core::Origin;
use slop_ninja_auth_core::account_wire::{AvatarType, Provider};
use slop_ninja_auth_core::pkce::Verifier;
use slop_ninja_auth_core::providers::{
    self, AVATAR_MAX, AuthorizeParams, ExchangeParams, OAuthProvider, PROFILE_BODY_MAX,
    ProviderError, TOKEN_BODY_MAX, X_AVATAR_HOST, api_get, authorize_url, parse_github_emails,
    parse_github_user, parse_token_response, parse_x_me, sniff_avatar, token_request, x_avatar_url,
};
use slop_ninja_auth_core::token::Token;

fn site() -> Origin {
    Origin::parse("https://site.example").unwrap()
}

#[test]
fn names_and_redirects() {
    for p in OAuthProvider::ALL {
        assert_eq!(OAuthProvider::parse(p.as_str()), Some(p));
        assert_eq!(p.spec().provider, p);
    }
    assert_eq!(OAuthProvider::parse("apple"), None);
    // The OAuth providers are exactly the contract's OAuth methods, under
    // the same wire strings and kill switches.
    for &m in Provider::ALL {
        assert_eq!(OAuthProvider::of(m).is_some(), m.is_oauth(), "{m:?}");
        if let Some(p) = OAuthProvider::of(m) {
            assert_eq!(p.method(), m);
            assert_eq!(p.as_str(), m.as_str());
            assert_eq!(p.switch(), m.switch());
        }
    }
    assert_eq!(OAuthProvider::parse("email"), None);
    assert_eq!(OAuthProvider::parse("bittensor"), None);
    assert_eq!(
        OAuthProvider::X.redirect_uri(&site()),
        "https://site.example/auth/x/callback"
    );
    // Every production endpoint is https on the provider's own host.
    for p in OAuthProvider::ALL {
        let e = p.spec().endpoints();
        for u in [
            Some(&e.authorize),
            Some(&e.token),
            e.user.as_ref(),
            e.emails.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let url = url::Url::parse(u).unwrap();
            assert_eq!(url.scheme(), "https", "{u}");
            assert!(url.port().is_none());
        }
    }
}

fn params<'a>(
    state: &'a str,
    nonce: &'a str,
    challenge: &'a str,
    redirect: &'a str,
) -> AuthorizeParams<'a> {
    AuthorizeParams {
        client_id: "cid",
        redirect_uri: redirect,
        state,
        nonce,
        code_challenge: challenge,
    }
}

#[test]
fn authorize_urls() {
    let state = Token::from_random([1; 32]).encode();
    let nonce = Token::from_random([2; 32]).encode();
    let challenge = Verifier::from_random([3; 32]).challenge();
    let s = site();

    let g = OAuthProvider::Google;
    let redirect = g.redirect_uri(&s);
    let url = authorize_url(
        g.spec(),
        &g.spec().endpoints(),
        &params(&state, &nonce, &challenge, &redirect),
    );
    assert_eq!(
        url,
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id=cid&redirect_uri=https%3A%2F%2Fsite.example%2Fauth%2Fgoogle%2Fcallback&scope=openid%20email%20profile&state={state}&nonce={nonce}&code_challenge={challenge}&code_challenge_method=S256&prompt=select_account"
        )
    );
    assert!(!url.contains("access_type"));

    let gh = OAuthProvider::GitHub;
    let redirect = gh.redirect_uri(&s);
    let url = authorize_url(
        gh.spec(),
        &gh.spec().endpoints(),
        &params(&state, &nonce, &challenge, &redirect),
    );
    assert_eq!(
        url,
        format!(
            "https://github.com/login/oauth/authorize?response_type=code&client_id=cid&redirect_uri=https%3A%2F%2Fsite.example%2Fauth%2Fgithub%2Fcallback&state={state}&code_challenge={challenge}&code_challenge_method=S256&prompt=select_account"
        )
    );
    assert!(!url.contains("nonce") && !url.contains("scope"));

    let x = OAuthProvider::X;
    let redirect = x.redirect_uri(&s);
    let url = authorize_url(
        x.spec(),
        &x.spec().endpoints(),
        &params(&state, &nonce, &challenge, &redirect),
    );
    assert_eq!(
        url,
        format!(
            "https://x.com/i/oauth2/authorize?response_type=code&client_id=cid&redirect_uri=https%3A%2F%2Fsite.example%2Fauth%2Fx%2Fcallback&scope=users.read%20tweet.read%20users.email&state={state}&code_challenge={challenge}&code_challenge_method=S256"
        )
    );
    assert!(!url.contains("offline"));
}

fn exchange<'a>(redirect: &'a str) -> ExchangeParams<'a> {
    ExchangeParams {
        client_id: "cid",
        client_secret: "sec",
        code: "the code",
        redirect_uri: redirect,
        code_verifier: "ver",
    }
}

#[test]
fn token_requests() {
    let s = site();
    let redirect = OAuthProvider::Google.redirect_uri(&s);
    let r = token_request(
        &providers::GOOGLE,
        &providers::GOOGLE.endpoints(),
        &exchange(&redirect),
    );
    assert_eq!(r.method, "POST");
    assert_eq!(r.url, "https://oauth2.googleapis.com/token");
    assert_eq!(
        r.body,
        "grant_type=authorization_code&code=the+code&redirect_uri=https%3A%2F%2Fsite.example%2Fauth%2Fgoogle%2Fcallback&code_verifier=ver&client_id=cid&client_secret=sec"
    );
    assert!(!r.headers.iter().any(|(k, _)| *k == "Authorization"));
    // Debug never prints the body.
    assert!(!format!("{r:?}").contains("sec"));

    let redirect = OAuthProvider::GitHub.redirect_uri(&s);
    let r = token_request(
        &providers::GITHUB,
        &providers::GITHUB.endpoints(),
        &exchange(&redirect),
    );
    assert_eq!(
        r.body,
        "code=the+code&redirect_uri=https%3A%2F%2Fsite.example%2Fauth%2Fgithub%2Fcallback&code_verifier=ver&client_id=cid&client_secret=sec"
    );
    assert!(
        r.headers
            .contains(&("Accept", "application/json".to_owned()))
    );

    let redirect = OAuthProvider::X.redirect_uri(&s);
    let r = token_request(
        &providers::X,
        &providers::X.endpoints(),
        &exchange(&redirect),
    );
    assert_eq!(r.url, "https://api.x.com/2/oauth2/token");
    assert_eq!(
        r.body,
        "grant_type=authorization_code&code=the+code&redirect_uri=https%3A%2F%2Fsite.example%2Fauth%2Fx%2Fcallback&code_verifier=ver"
    );
    // base64("cid:sec")
    assert!(
        r.headers
            .contains(&("Authorization", "Basic Y2lkOnNlYw==".to_owned()))
    );

    let api = api_get(&providers::GITHUB, "https://api.github.com/user", "tok");
    assert!(
        api.headers
            .contains(&("Authorization", "Bearer tok".to_owned()))
    );
    assert!(api.headers.iter().any(|(k, _)| *k == "User-Agent"));
}

#[test]
fn token_responses() {
    let g = &providers::GOOGLE;
    let ok =
        br#"{"access_token":"ya29.a","token_type":"Bearer","id_token":"h.p.s","expires_in":3599}"#;
    let t = parse_token_response(g, 200, ok).unwrap();
    assert_eq!(t.access_token, "ya29.a");
    assert_eq!(t.id_token.as_deref(), Some("h.p.s"));
    assert_eq!(format!("{t:?}"), "TokenResponse(..)");
    // Google without an ID token is malformed.
    assert_eq!(
        parse_token_response(g, 200, br#"{"access_token":"a","token_type":"Bearer"}"#),
        Err(ProviderError::Malformed)
    );
    assert_eq!(
        parse_token_response(g, 400, br#"{"error":"invalid_grant"}"#),
        Err(ProviderError::Refused)
    );
    // GitHub reports errors in a 200 body.
    let gh = &providers::GITHUB;
    assert_eq!(
        parse_token_response(
            gh,
            200,
            br#"{"error":"bad_verification_code","error_description":"x"}"#
        ),
        Err(ProviderError::Refused)
    );
    assert_eq!(
        parse_token_response(
            gh,
            200,
            br#"{"access_token":"ghu_1","token_type":"bearer","scope":""}"#
        )
        .unwrap()
        .id_token,
        None
    );
    // X refusing for now (a rate limit and the like).
    let x = &providers::X;
    assert_eq!(
        parse_token_response(x, 429, b"{}"),
        Err(ProviderError::Unavailable)
    );
    assert_eq!(
        parse_token_response(x, 402, b""),
        Err(ProviderError::Unavailable)
    );
    assert_eq!(
        parse_token_response(x, 500, b""),
        Err(ProviderError::Upstream)
    );
    assert_eq!(
        parse_token_response(x, 200, br#"{"access_token":"a b","token_type":"bearer"}"#),
        Err(ProviderError::Malformed)
    );
    assert_eq!(
        parse_token_response(x, 200, br#"{"access_token":"a","token_type":"mac"}"#),
        Err(ProviderError::Malformed)
    );
    assert_eq!(
        parse_token_response(x, 200, b"not json"),
        Err(ProviderError::Malformed)
    );
    let big = vec![b' '; TOKEN_BODY_MAX + 1];
    assert_eq!(
        parse_token_response(x, 200, &big),
        Err(ProviderError::TooLarge)
    );
}

#[test]
fn github_profile() {
    let u = parse_github_user(200, br#"{"login":"octo-cat","id":583231,"name":"x"}"#).unwrap();
    assert_eq!((u.id, u.login.as_str()), (583_231, "octo-cat"));
    for bad in [
        &br#"{"login":"octo","id":0}"#[..],
        br#"{"login":"oc to","id":1}"#,
        br#"{"login":"octo"}"#,
        br#"{"id":-1,"login":"octo"}"#,
        br#"{"id":"1","login":"octo"}"#,
    ] {
        assert_eq!(parse_github_user(200, bad), Err(ProviderError::Malformed));
    }
    assert_eq!(parse_github_user(401, b"{}"), Err(ProviderError::Refused));

    let emails = br#"[
        {"email":"Second@Example.com","primary":false,"verified":true,"visibility":null},
        {"email":"Octo@Example.com","primary":true,"verified":true,"visibility":"public"}
    ]"#;
    let e = parse_github_emails(200, emails).unwrap().unwrap();
    assert_eq!(e.norm.as_str(), "octo@example.com");
    // Primary but unverified, or a noreply placeholder, is no address.
    let unverified = br#"[{"email":"a@example.com","primary":true,"verified":false}]"#;
    assert_eq!(parse_github_emails(200, unverified), Ok(None));
    let noreply =
        br#"[{"email":"1+octo@users.noreply.github.com","primary":true,"verified":true}]"#;
    assert_eq!(parse_github_emails(200, noreply), Ok(None));
    assert_eq!(parse_github_emails(200, b"[]"), Ok(None));
    assert_eq!(
        parse_github_emails(200, b"{}"),
        Err(ProviderError::Malformed)
    );
    let big = vec![b' '; PROFILE_BODY_MAX + 1];
    assert_eq!(parse_github_emails(200, &big), Err(ProviderError::TooLarge));
}

#[test]
fn x_profile() {
    let body = br#"{"data":{"id":"1234567890","username":"SlopFan_1","name":"Slop Fan","profile_image_url":"https://pbs.twimg.com/profile_images/1/a_normal.jpg","confirmed_email":"Fan@Example.com","verified_type":"blue"}}"#;
    let u = parse_x_me(200, body).unwrap();
    assert_eq!(u.id, "1234567890");
    assert_eq!(u.username, "SlopFan_1");
    assert_eq!(u.name.as_deref(), Some("Slop Fan"));
    assert!(u.profile_image_url.is_some());
    assert_eq!(u.confirmed_email.unwrap().norm.as_str(), "fan@example.com");
    assert_eq!(u.verified_type.as_deref(), Some("blue"));

    // A missing email is allowed; a foreign avatar host is dropped; a long
    // or control-laden name is dropped; an unknown verified type is dropped.
    let body = format!(
        r#"{{"data":{{"id":"7","username":"a","name":"{}","profile_image_url":"https://evil.example/a.jpg","verified_type":"gold"}}}}"#,
        "n".repeat(51)
    );
    let u = parse_x_me(200, body.as_bytes()).unwrap();
    assert_eq!(
        (
            u.name,
            u.profile_image_url,
            u.confirmed_email,
            u.verified_type
        ),
        (None, None, None, None)
    );
    for bad in [
        &br#"{"data":{"id":"x1","username":"a"}}"#[..],
        br#"{"data":{"id":"01","username":"a"}}"#,
        br#"{"data":{"id":"1","username":"has space"}}"#,
        br#"{"data":{"id":"1","username":"waytoolongusername"}}"#,
        br#"{"errors":[{"title":"x"}]}"#,
    ] {
        assert_eq!(parse_x_me(200, bad), Err(ProviderError::Malformed));
    }
    assert_eq!(parse_x_me(429, b"{}"), Err(ProviderError::Unavailable));
}

#[test]
fn x_avatar() {
    assert!(x_avatar_url("https://pbs.twimg.com/profile_images/1/a_normal.jpg").is_some());
    let host = X_AVATAR_HOST;
    for bad in [
        format!("http://{host}/a.jpg"),
        format!("https://{host}:8443/a.jpg"),
        // User info before the host.
        format!("https://user@{host}/a.jpg"),
        format!("https://{host}.evil.example/a.jpg"),
        "https://abs.twimg.com/a.jpg".to_owned(),
        // The host as user info before another host.
        format!("https://{host}@evil.example/a.jpg"),
        "data:image/png;base64,AAAA".to_owned(),
        format!("//{host}/a.jpg"),
    ] {
        assert!(x_avatar_url(&bad).is_none(), "{bad}");
    }
    // Upper-case hosts normalize to the same host and pass.
    assert!(x_avatar_url("https://PBS.TWIMG.COM/a.jpg").is_some());

    assert_eq!(
        sniff_avatar(&[0xFF, 0xD8, 0xFF, 0xE0]),
        Some(AvatarType::Jpeg)
    );
    assert_eq!(
        sniff_avatar(b"\x89PNG\r\n\x1a\n...."),
        Some(AvatarType::Png)
    );
    assert_eq!(sniff_avatar(b"GIF89a.."), Some(AvatarType::Gif));
    assert_eq!(
        sniff_avatar(b"RIFF\0\0\0\0WEBPVP8 "),
        Some(AvatarType::Webp)
    );
    for bad in [
        &b"<svg xmlns='http://www.w3.org/2000/svg'/>"[..],
        b"<!doctype html><html>",
        b"RIFF\0\0\0\0WAVE",
        b"",
    ] {
        assert_eq!(sniff_avatar(bad), None);
    }
    let mut big = vec![0xFF, 0xD8, 0xFF];
    big.resize(AVATAR_MAX + 1, 0);
    assert_eq!(sniff_avatar(&big), None);
    assert_eq!(AvatarType::Webp.as_str(), "image/webp");
}

#[cfg(feature = "dev")]
#[test]
fn dev_endpoints_move_under_the_stub() {
    let stub = Origin::parse("https://stub.localhost:8790").unwrap();
    let e = providers::X.endpoints_at(&stub);
    assert_eq!(
        e.authorize,
        "https://stub.localhost:8790/x.com/i/oauth2/authorize"
    );
    assert_eq!(
        e.token,
        "https://stub.localhost:8790/api.x.com/2/oauth2/token"
    );
    assert_eq!(
        e.user.as_deref(),
        Some(
            "https://stub.localhost:8790/api.x.com/2/users/me?user.fields=confirmed_email,profile_image_url,username,name,verified_type"
        )
    );
    let g = providers::GITHUB.endpoints_at(&stub);
    assert_eq!(
        g.emails.as_deref(),
        Some("https://stub.localhost:8790/api.github.com/user/emails")
    );
}

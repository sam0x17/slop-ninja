use super::*;
use slop_ninja_account_wire::AvatarType;
use slop_ninja_dev_http::pairs;

/// A PKCE verifier of the allowed form (43 to 128 unreserved characters).
const VERIFIER: &str = "verifier-verifier-verifier-verifier-verifier";
const SITE: &str = DEV_SITE_ORIGIN;

fn cfg() -> Config {
    Config::from_vars(|_| None).expect("the dev defaults parse")
}

fn challenge() -> String {
    b64url(&Sha256::digest(VERIFIER.as_bytes()))
}

fn reply(o: Outcome) -> Reply {
    match o {
        Outcome::Reply(r) => r,
        other => panic!("{other:?}"),
    }
}

fn redirect_uri(p: Provider) -> String {
    format!("{SITE}/auth/{}/callback", p.name())
}

fn authorize_path(p: Provider) -> &'static str {
    match p {
        Provider::Google => "/google/o/oauth2/v2/auth",
        Provider::Github => "/github/login/oauth/authorize",
        Provider::X => "/x/i/oauth2/authorize",
    }
}

fn authorize_call(p: Provider, extra: &[(&str, &str)]) -> Call {
    let c = cfg();
    let ch = challenge();
    let ru = redirect_uri(p);
    let mut f: Vec<(&str, &str)> = vec![
        ("client_id", c.client(p).id.as_str()),
        ("redirect_uri", ru.as_str()),
        ("response_type", "code"),
        ("state", "st-1"),
        ("code_challenge", ch.as_str()),
        ("code_challenge_method", "S256"),
    ];
    f.push((
        "scope",
        match p {
            Provider::Google => "openid email profile",
            Provider::Github => "",
            Provider::X => "users.read tweet.read users.email",
        },
    ));
    if p == Provider::Google {
        f.push(("nonce", "n-1"));
    }
    for (k, v) in extra {
        f.retain(|(x, _)| x != k);
        f.push((k, v));
    }
    let owned: Vec<(String, String)> = f.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let refs: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Call::new("GET", &format!("{}?{}", authorize_path(p), encode(&refs)))
}

/// The redirect's query, from a 302.
fn back(r: &Reply) -> Vec<(String, String)> {
    assert_eq!(r.status, 302, "{}", r.body_text());
    let loc = r.header("location").unwrap_or("");
    pairs(loc.split_once('?').map_or("", |(_, q)| q).as_bytes())
}

fn code_of(s: &mut Stub, p: Provider, extra: &[(&str, &str)], now: u64) -> String {
    let r = reply(s.handle(&cfg(), &authorize_call(p, extra), now));
    let q = back(&r);
    assert_eq!(field(&q, "state"), Some("st-1"));
    field(&q, "code").expect("code").to_owned()
}

fn google_token(s: &mut Stub, code: &str, verifier: &str, now: u64) -> Outcome {
    let c = cfg();
    let ru = redirect_uri(Provider::Google);
    let call = Call::new("POST", "/google/token").form(&[
        ("code", code),
        ("client_id", &c.google.id),
        ("client_secret", &c.google.secret),
        ("redirect_uri", &ru),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier),
    ]);
    s.handle(&c, &call, now)
}

fn switch(s: &mut Stub, v: Value) -> Reply {
    reply(s.handle(&cfg(), &Call::new("POST", "/__stub/switch").json(&v), 0))
}

#[test]
fn google_code_flow_with_pkce_and_claims() {
    let mut s = Stub::default();
    let code = code_of(&mut s, Provider::Google, &[], 1_000);
    match google_token(&mut s, &code, VERIFIER, 2_000) {
        Outcome::IdToken { claims, body, delay_ms } => {
            assert_eq!(delay_ms, 0);
            assert_eq!(claims["iss"], "https://accounts.google.com");
            assert_eq!(claims["aud"], cfg().google.id);
            assert_eq!(claims["sub"], "110000000000000000001");
            assert_eq!(claims["email"], "alice@example.com");
            assert_eq!(claims["email_verified"], true);
            assert_eq!(claims["nonce"], "n-1");
            assert_eq!(claims["exp"].as_u64(), Some(2 + 3600));
            assert!(body["access_token"].as_str().is_some_and(|t| t.starts_with("ya29.")));
        }
        other => panic!("{other:?}"),
    }
    // The code is single use.
    let again = reply(google_token(&mut s, &code, VERIFIER, 3_000));
    assert_eq!((again.status, again.body_json()["error"].clone()), (400, json!("invalid_grant")));
}

#[test]
fn google_refusals() {
    let mut s = Stub::default();
    let code = code_of(&mut s, Provider::Google, &[], 0);
    let bad = reply(google_token(&mut s, &code, &VERIFIER.replace('v', "w"), 1));
    assert_eq!(bad.body_json()["error_description"], "Invalid code verifier.");
    // A refused exchange spends the code.
    assert_eq!(reply(google_token(&mut s, &code, VERIFIER, 2)).status, 400);
    // Wrong secret.
    let code = code_of(&mut s, Provider::Google, &[], 0);
    let c = cfg();
    let call = Call::new("POST", "/google/token").form(&[("code", &code), ("client_id", &c.google.id), ("client_secret", "nope"), ("grant_type", "authorization_code")]);
    assert_eq!(reply(s.handle(&c, &call, 1)).status, 401);
    // Unregistered redirect URI and a missing challenge never reach the redirect.
    let r = reply(s.handle(&c, &authorize_call(Provider::Google, &[("redirect_uri", "https://evil.example/cb")]), 0));
    assert_eq!(r.status, 400);
    let r = reply(s.handle(&c, &authorize_call(Provider::Google, &[("code_challenge", "")]), 0));
    assert_eq!(r.status, 400);
    // An expired code.
    let code = code_of(&mut s, Provider::Google, &[], 0);
    assert_eq!(reply(google_token(&mut s, &code, VERIFIER, 11 * 60_000)).status, 400);
}

#[test]
fn google_switches_and_hints() {
    let mut s = Stub::default();
    let code = code_of(&mut s, Provider::Google, &[("login_hint", "carol@example.org")], 0);
    let Outcome::IdToken { claims, .. } = google_token(&mut s, &code, VERIFIER, 1) else { panic!() };
    assert_eq!((claims["email"].as_str(), claims["email_verified"].as_bool()), (Some("carol@example.org"), Some(false)));
    for (fault, key, want) in [
        ("iss_bare", "iss", json!("accounts.google.com")),
        ("iss_wrong", "iss", json!("https://accounts.example.com")),
        ("aud_wrong", "aud", json!("another-client.apps.example.com")),
        ("nonce_wrong", "nonce", json!("not-the-nonce")),
        ("nonce_missing", "nonce", Value::Null),
    ] {
        assert_eq!(switch(&mut s, json!({"provider": "google", "id_token": fault})).status, 200);
        let code = code_of(&mut s, Provider::Google, &[], 0);
        let Outcome::IdToken { claims, .. } = google_token(&mut s, &code, VERIFIER, 1) else { panic!() };
        assert_eq!(claims[key], want, "{fault}");
    }
    switch(&mut s, json!({"provider": "google", "id_token": "expired", "email": "missing", "user": "bob", "token_delay_ms": 5}));
    let code = code_of(&mut s, Provider::Google, &[], 9_999_000);
    let Outcome::IdToken { claims, delay_ms, .. } = google_token(&mut s, &code, VERIFIER, 10_000_000) else { panic!() };
    assert!(claims["exp"].as_u64() < Some(10_000));
    assert!(claims.get("email").is_none() && claims.get("email_verified").is_none());
    assert_eq!((claims["sub"].as_str(), delay_ms), (Some("110000000000000000002"), 5));
    // Deny, then the chooser and its consent route.
    switch(&mut s, json!({"provider": "google", "deny": true}));
    let q = back(&reply(s.handle(&cfg(), &authorize_call(Provider::Google, &[]), 0)));
    assert_eq!((field(&q, "error"), field(&q, "state")), (Some("access_denied"), Some("st-1")));
    switch(&mut s, json!({"provider": "google", "deny": false, "chooser": true}));
    let page = reply(s.handle(&cfg(), &authorize_call(Provider::Google, &[]), 0));
    assert!(page.status == 200 && page.body_text().contains("value=\"dave\""));
    let mut consent = authorize_call(Provider::Google, &[("user", "dave")]);
    consent.path = "/google/consent".into();
    let q = back(&reply(s.handle(&cfg(), &consent, 0)));
    assert!(field(&q, "code").is_some());
    // Unknown switch fields are refused; reset restores every default.
    assert_eq!(switch(&mut s, json!({"provider": "google", "nonsense": 1})).status, 400);
    assert_eq!(switch(&mut s, json!({"provider": "nobody"})).status, 400);
    reply(s.handle(&cfg(), &Call::new("POST", "/__stub/reset"), 0));
    assert_eq!(*s.switches(Provider::Google), ProviderSwitches::default());
}

#[test]
fn discovery_names_the_stub_endpoints() {
    let mut s = Stub::default();
    let d = reply(s.handle(&cfg(), &Call::new("GET", "/google/.well-known/openid-configuration"), 0)).body_json();
    assert_eq!(d["token_endpoint"], format!("{DEV_STUB_ORIGIN}/google/token"));
    assert_eq!(d["code_challenge_methods_supported"], json!(["plain", "S256"]));
    assert_eq!(s.handle(&cfg(), &Call::new("GET", "/google/oauth2/v3/certs"), 0), Outcome::Jwks);
}

fn github_exchange(s: &mut Stub, code: &str, json_accept: bool) -> Reply {
    let c = cfg();
    let ru = redirect_uri(Provider::Github);
    let mut call = Call::new("POST", "/github/login/oauth/access_token").form(&[
        ("client_id", &c.github.id),
        ("client_secret", &c.github.secret),
        ("code", code),
        ("redirect_uri", &ru),
        ("code_verifier", VERIFIER),
    ]);
    if json_accept {
        call = call.header("accept", "application/json");
    }
    reply(s.handle(&c, &call, 1))
}

fn github_get(s: &mut Stub, path: &str, token: &str, ua: bool) -> Reply {
    let mut call = Call::new("GET", path).header("authorization", &format!("Bearer {token}"));
    if ua {
        call = call.header("user-agent", "slop-ninja-dev");
    }
    reply(s.handle(&cfg(), &call, 2))
}

#[test]
fn github_flow_errors_and_emails() {
    let mut s = Stub::default();
    // Only S256.
    assert_eq!(reply(s.handle(&cfg(), &authorize_call(Provider::Github, &[("code_challenge_method", "plain")]), 0)).status, 400);
    let code = code_of(&mut s, Provider::Github, &[], 0);
    let t = github_exchange(&mut s, &code, true).body_json();
    let token = t["access_token"].as_str().expect("token").to_owned();
    assert_eq!(t["token_type"], "bearer");
    // A bad code answers 200 with an error field; form-encoded without Accept.
    let bad = github_exchange(&mut s, "nope", false);
    assert_eq!(bad.status, 200);
    assert!(bad.body_text().starts_with("error=bad_verification_code"));
    // The API needs a User-Agent and a live token.
    assert_eq!(github_get(&mut s, "/github/api/user", &token, false).status, 403);
    assert_eq!(github_get(&mut s, "/github/api/user", "ghu_nope", true).status, 401);
    let u = github_get(&mut s, "/github/api/user", &token, true).body_json();
    assert_eq!((u["id"].as_u64(), u["login"].as_str()), (Some(9_000_001), Some("alice-example")));
    let e = github_get(&mut s, "/github/api/user/emails", &token, true).body_json();
    assert_eq!(e[0]["primary"], false, "a non-primary verified address comes first");
    assert_eq!((e[1]["email"].as_str(), e[1]["primary"].as_bool(), e[1]["verified"].as_bool()), (Some("alice@example.com"), Some(true), Some(true)));
    for (mode, check) in [
        ("unverified", json!(false)),
        ("missing", Value::Null),
        ("placeholder", json!("9000001+alice-example@users.noreply.github.com")),
    ] {
        switch(&mut s, json!({"provider": "github", "email": mode}));
        let e = github_get(&mut s, "/github/api/user/emails", &token, true).body_json();
        match mode {
            "unverified" => assert_eq!(e[1]["verified"], check),
            "missing" => assert_eq!(e, json!([])),
            _ => assert_eq!(e[0]["email"], check),
        }
    }
    switch(&mut s, json!({"provider": "github", "userinfo_status": 500}));
    assert_eq!(github_get(&mut s, "/github/api/user", &token, true).status, 500);
}

fn x_exchange(s: &mut Stub, code: &str, basic_ok: bool, now: u64) -> Reply {
    let c = cfg();
    let ru = redirect_uri(Provider::X);
    let creds = if basic_ok { format!("{}:{}", c.x.id, c.x.secret) } else { format!("{}:wrong", c.x.id) };
    let call = Call::new("POST", "/x/2/oauth2/token")
        .header("authorization", &format!("Basic {}", STANDARD.encode(creds)))
        .form(&[("code", code), ("grant_type", "authorization_code"), ("redirect_uri", &ru), ("code_verifier", VERIFIER)]);
    reply(s.handle(&c, &call, now))
}

#[test]
fn x_flow_thirty_seconds_fields_and_avatars() {
    let mut s = Stub::default();
    // The scopes must include users.read and tweet.read.
    let q = back(&reply(s.handle(&cfg(), &authorize_call(Provider::X, &[("scope", "users.email")]), 0)));
    assert_eq!(field(&q, "error"), Some("invalid_scope"));
    let code = code_of(&mut s, Provider::X, &[], 0);
    assert_eq!(x_exchange(&mut s, &code, false, 1).status, 401);
    let code = code_of(&mut s, Provider::X, &[], 0);
    let late = x_exchange(&mut s, &code, true, 30_001);
    assert_eq!((late.status, late.body_json()["error"].clone()), (400, json!("invalid_request")));
    switch(&mut s, json!({"provider": "x", "token_delay_ms": 31_000}));
    let code = code_of(&mut s, Provider::X, &[], 0);
    let ok = x_exchange(&mut s, &code, true, 29_999);
    assert_eq!((ok.status, ok.delay_ms), (200, 31_000));
    let token = ok.body_json()["access_token"].as_str().expect("token").to_owned();
    let me = |s: &mut Stub, fields: &str| {
        let call = Call::new("GET", &format!("/x/2/users/me?user.fields={fields}")).header("authorization", &format!("Bearer {token}"));
        reply(s.handle(&cfg(), &call, 40_000))
    };
    let d = me(&mut s, "").body_json()["data"].clone();
    assert_eq!(d, json!({"id": "1900000000000000001", "name": "Alice Example", "username": "alice_example"}));
    let d = me(&mut s, "confirmed_email,profile_image_url,username,name,verified_type").body_json()["data"].clone();
    assert_eq!(d["confirmed_email"], "alice@example.com");
    assert_eq!(d["profile_image_url"], "https://pbs.twimg.com/profile_images/1900000000000000001/alice_normal.png");
    assert_eq!(d["verified_type"], "none");
    switch(&mut s, json!({"provider": "x", "userinfo_status": 402}));
    assert_eq!(me(&mut s, "").body_json()["title"], "CreditsDepleted");
    // Avatars by kind.
    let av = |s: &mut Stub, key: &str| reply(s.handle(&cfg(), &Call::new("GET", &format!("/x/pbs/profile_images/1/{key}_normal.png")), 0));
    assert_eq!(av(&mut s, "alice").body.to_vec(), users::PNG_1X1);
    assert_eq!(av(&mut s, "bob").header("content-type"), Some("image/jpeg"));
    assert!(av(&mut s, "carol").body_text().starts_with("<svg"));
    assert!(av(&mut s, "erin").body.len() > 64 * 1024);
    switch(&mut s, json!({"provider": "x", "avatar": "redirect"}));
    assert_eq!(av(&mut s, "alice").status, 302);
    switch(&mut s, json!({"provider": "x", "avatar": "missing"}));
    assert_eq!(av(&mut s, "alice").status, 404);
}

#[test]
fn x_email_needs_the_scope_and_a_confirmed_address() {
    let mut s = Stub::default();
    let code = code_of(&mut s, Provider::X, &[("scope", "users.read tweet.read")], 0);
    let token = x_exchange(&mut s, &code, true, 1).body_json()["access_token"].as_str().unwrap_or("").to_owned();
    let call = Call::new("GET", "/x/2/users/me?user.fields=confirmed_email").header("authorization", &format!("Bearer {token}"));
    assert!(reply(s.handle(&cfg(), &call, 2)).body_json()["data"].get("confirmed_email").is_none());
    let revoke = Call::new("POST", "/x/2/oauth2/revoke")
        .header("authorization", &format!("Basic {}", STANDARD.encode(format!("{}:{}", cfg().x.id, cfg().x.secret))))
        .form(&[("token", &token)]);
    assert_eq!(reply(s.handle(&cfg(), &revoke, 3)).status, 200);
    assert_eq!(reply(s.handle(&cfg(), &call, 4)).status, 401);
}

#[test]
fn mail_capture_and_its_switches() {
    let mut s = Stub::default();
    let c = cfg();
    let send = |s: &mut Stub, to: &str| {
        let v = json!({"to": to, "from": "SlopNinja <sender@example.com>", "subject": "Your SlopNinja sign-in link",
            "text": format!("Sign in to SlopNinja: {SITE}/auth/email/confirm#t=abc123 The link works once."), "headers": {"Auto-Submitted": "auto-generated"}});
        reply(s.handle(&cfg(), &Call::new("POST", "/mail").json(&v), 5))
    };
    assert_eq!(send(&mut s, "alice@example.com").status, 201);
    assert_eq!(send(&mut s, "bob@example.com").status, 201);
    let latest = reply(s.handle(&c, &Call::new("GET", "/mail/latest?to=alice%40example.com"), 6)).body_json();
    assert_eq!(latest["links"], json!([format!("{SITE}/auth/email/confirm#t=abc123")]));
    assert_eq!(latest["to"], "alice@example.com");
    let all = reply(s.handle(&c, &Call::new("GET", "/mail"), 6)).body_json();
    assert_eq!(all["messages"].as_array().map(Vec::len), Some(2));
    assert_eq!(reply(s.handle(&c, &Call::new("GET", "/mail/latest?to=carol%40example.org"), 6)).status, 404);
    switch(&mut s, json!({"provider": "mail", "suppressed": ["carol@example.org"]}));
    assert_eq!(send(&mut s, "carol@example.org").body_json()["error"], "E_RECIPIENT_SUPPRESSED");
    switch(&mut s, json!({"provider": "mail", "error": "E_DAILY_LIMIT_EXCEEDED"}));
    assert_eq!(send(&mut s, "alice@example.com").status, 422);
    assert_eq!(reply(s.handle(&c, &Call::new("POST", "/mail").json(&json!({"to": "a"})), 7)).status, 400);
    reply(s.handle(&c, &Call::new("POST", "/mail/clear"), 8));
    assert_eq!(s.mail.len(), 0);
    let hits = reply(s.handle(&c, &Call::new("GET", "/__stub/state"), 9)).body_json()["hits"]["mail"].clone();
    assert_eq!(hits, 5);
}

#[test]
fn pkce_checks_the_verifier_form() {
    assert!(pkce_ok(VERIFIER, &challenge(), "S256"));
    assert!(!pkce_ok("short", &b64url(&Sha256::digest(b"short")), "S256"));
    assert!(!pkce_ok(&"a".repeat(43), &"a".repeat(43), "S384"));
    assert!(pkce_ok(&"a".repeat(43), &"a".repeat(43), "plain"));
    // The challenge is the unpadded base64url SHA-256 of the verifier.
    assert_eq!(challenge().len(), 43);
    assert!(!pkce_ok(VERIFIER, &b64url(&Sha256::digest(b"another verifier")), "S256"));
}

#[test]
fn providers_are_the_account_contracts_oauth_methods() {
    let oauth: Vec<Method> = Method::ALL.iter().copied().filter(|m| m.is_oauth()).collect();
    assert_eq!(Provider::ALL.map(Provider::method).to_vec(), oauth);
    for p in Provider::ALL {
        assert_eq!(Provider::parse(p.name()), Some(p));
        assert_eq!(serde_json::to_value(p).ok(), Some(json!(p.method().as_str())), "stored state keeps the wire string");
    }
    assert_eq!(Provider::of_method(Method::Email), None);
    assert_eq!(Provider::of_method(Method::Bittensor), None);
    // A switch names a provider by its method; email and wallet are not stubbed here.
    let mut s = Stub::default();
    let r = reply(s.handle(&cfg(), &Call::new("POST", "/__stub/switch").json(&json!({"provider": "bittensor", "deny": true})), 0));
    assert_eq!(r.status, 400);
}

#[test]
fn origins_parse_as_the_workers_parse_theirs() {
    let c = cfg();
    assert_eq!(c.origin.as_str(), DEV_STUB_ORIGIN);
    for p in Provider::ALL {
        assert_eq!(c.client(p).redirect_uris, vec![redirect_uri(p)]);
    }
    let with = |k: &'static str, v: &'static str| Config::from_vars(move |q| (q == k).then(|| v.to_owned()));
    let moved = with("SITE_ORIGIN", "https://site.example:9443").expect("an origin");
    assert_eq!(moved.github.redirect_uris, vec!["https://site.example:9443/auth/github/callback".to_owned()]);
    for (k, bad) in [
        ("SITE_ORIGIN", "http://slop.localhost:8787"),
        ("SITE_ORIGIN", "https://slop.localhost:8787/"),
        ("SITE_ORIGIN", "https://Slop.localhost"),
        ("STUB_ORIGIN", "https://auth.slop.localhost:08789"),
    ] {
        assert!(with(k, bad).is_err(), "{k} {bad}");
    }
}

#[test]
fn avatar_labels_are_the_stored_types() {
    for a in Avatar::ALL {
        match (a.stored(), users::avatar_bytes(a)) {
            // A kept answer is labelled with the type its bytes are.
            (Some(t), Some((bytes, label))) => {
                assert_eq!(label, t.as_str(), "{a:?}");
                let magic: &[u8] = match t {
                    AvatarType::Png => b"\x89PNG",
                    AvatarType::Jpeg => b"\xFF\xD8\xFF",
                    AvatarType::Gif => b"GIF8",
                    AvatarType::Webp => b"RIFF",
                };
                assert!(bytes.starts_with(magic) && bytes.len() <= 64 * 1024, "{a:?}");
            }
            // A dropped one is mislabelled, oversized, or no image at all.
            (None, Some((bytes, label))) => {
                assert_eq!(label, AvatarType::Png.as_str(), "{a:?}");
                assert!(!bytes.starts_with(b"\x89PNG") || bytes.len() > 64 * 1024, "{a:?}");
            }
            (None, None) => assert!(matches!(a, Avatar::Redirect | Avatar::Missing)),
            (Some(_), None) => panic!("{a:?}: a stored type needs bytes"),
        }
    }
}

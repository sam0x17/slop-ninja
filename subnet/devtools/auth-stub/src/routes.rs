//! The auth stub's routes as pure functions of a call and the stub's state.
//!
//! The stub is stricter than the providers where SlopNinja's flows promise
//! more than a provider requires: every authorize must carry `state` and a
//! PKCE `code_challenge`, every redirect URI must be one registered exactly,
//! and every exchange must present the matching verifier and client secret.
//! Where a provider's own behaviour is the point (GitHub's 200 answer to a
//! bad code, its `User-Agent` rule, X's 30-second code life, X's fields
//! returned only when asked), the stub follows the provider.

use crate::users::{self, Avatar, User};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_dev_http::{Call, Reply, encode, escape, field};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Google,
    Github,
    X,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Github => "github",
            Provider::X => "x",
        }
    }

    /// How long an issued code may be exchanged.
    fn code_ttl_ms(self) -> u64 {
        match self {
            Provider::Google | Provider::Github => 10 * 60_000,
            // X: "within 30 seconds" of approval.
            Provider::X => 30_000,
        }
    }

    fn token_ttl_ms(self) -> u64 {
        match self {
            Provider::Google => 3_600_000,
            Provider::Github => 8 * 3_600_000,
            Provider::X => 2 * 3_600_000,
        }
    }
}

/// One registered OAuth client.
#[derive(Clone, Debug)]
pub struct Client {
    pub id: String,
    pub secret: String,
    pub redirect_uris: Vec<String>,
}

/// The stub's configuration (Worker vars; every value has a dev default).
#[derive(Clone, Debug)]
pub struct Config {
    /// The stub's own origin, as the browser reaches it.
    pub origin: String,
    pub google: Client,
    pub github: Client,
    pub x: Client,
}

/// The default site origin of the dev session.
pub const DEV_SITE_ORIGIN: &str = "https://slop.localhost:8787";
/// The default origin of the stubs session.
pub const DEV_STUB_ORIGIN: &str = "https://auth.slop.localhost:8789";

impl Config {
    /// From vars: `STUB_ORIGIN`, `SITE_ORIGIN` and, per provider `P` in
    /// `GOOGLE`, `GITHUB` and `X`, `P_CLIENT_ID`, `P_CLIENT_SECRET` and
    /// `P_REDIRECT_URIS` (comma-separated; default
    /// `<SITE_ORIGIN>/auth/<p>/callback`).
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Config {
        let get = |k: &str| get(k).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let site = get("SITE_ORIGIN").unwrap_or_else(|| DEV_SITE_ORIGIN.to_owned());
        let client = |p: Provider| {
            let up = p.name().to_ascii_uppercase();
            Client {
                id: get(&format!("{up}_CLIENT_ID")).unwrap_or_else(|| format!("dev-{}-client-id", p.name())),
                secret: get(&format!("{up}_CLIENT_SECRET")).unwrap_or_else(|| format!("dev-{}-client-secret-not-real", p.name())),
                redirect_uris: get(&format!("{up}_REDIRECT_URIS"))
                    .map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_else(|| vec![format!("{site}/auth/{}/callback", p.name())]),
            }
        };
        Config {
            origin: get("STUB_ORIGIN").unwrap_or_else(|| DEV_STUB_ORIGIN.to_owned()),
            google: client(Provider::Google),
            github: client(Provider::Github),
            x: client(Provider::X),
        }
    }

    pub fn client(&self, p: Provider) -> &Client {
        match p {
            Provider::Google => &self.google,
            Provider::Github => &self.github,
            Provider::X => &self.x,
        }
    }
}

/// How a provider reports the user's address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailMode {
    /// No address at all.
    Missing,
    /// The address, marked unverified.
    Unverified,
    /// GitHub: a `users.noreply.github.com` placeholder as the primary address.
    Placeholder,
}

/// A fault in Google's ID token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdTokenFault {
    /// `iss` without the scheme (`accounts.google.com`, which is valid).
    IssBare,
    IssWrong,
    AudWrong,
    NonceWrong,
    NonceMissing,
    Expired,
}

/// Per-provider failure switches.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSwitches {
    /// The test user an authorize consents as (default `alice`; Google's
    /// `login_hint` wins when it names a test user).
    pub user: Option<String>,
    /// Show the consent chooser instead of consenting at once.
    pub chooser: bool,
    /// The consent is refused (`error=access_denied`).
    pub deny: bool,
    /// The token endpoint answers this OAuth error code.
    pub token_error: Option<String>,
    /// The token endpoint answers after this many ms (X's slow exchange:
    /// more than 30,000).
    pub token_delay_ms: u64,
    /// The user endpoints answer this status (X credits: 402; limits: 429).
    pub userinfo_status: Option<u16>,
    pub email: Option<EmailMode>,
    /// Google only.
    pub id_token: Option<IdTokenFault>,
    /// X only: what every avatar fetch serves.
    pub avatar: Option<Avatar>,
}

/// Mail capture switches.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MailSwitches {
    /// Every send fails with this Email Sending code (`E_RATE_LIMIT_EXCEEDED`,
    /// `E_DAILY_LIMIT_EXCEEDED`, ...).
    pub error: Option<String>,
    /// Sends to these addresses fail with `E_RECIPIENT_SUPPRESSED`.
    pub suppressed: Vec<String>,
    pub delay_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Code {
    pub code: String,
    pub provider: Provider,
    pub client_id: String,
    pub redirect_uri: String,
    pub challenge: String,
    pub method: String,
    pub scope: String,
    pub nonce: Option<String>,
    pub user: String,
    pub issued_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub token: String,
    pub provider: Provider,
    pub user: String,
    pub scope: String,
    pub issued_ms: u64,
}

/// One captured mail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mail {
    pub id: u64,
    pub received_ms: u64,
    pub to: String,
    pub from: Option<String>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub headers: Value,
    /// The `http(s)` links in the text part, in order.
    pub links: Vec<String>,
}

/// The most mails kept; older ones are dropped first.
pub const MAX_MAIL: usize = 500;

/// The stub's whole state, kept between requests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stub {
    pub seq: u64,
    pub codes: Vec<Code>,
    pub tokens: Vec<Token>,
    pub mail: Vec<Mail>,
    pub mail_seq: u64,
    pub google: ProviderSwitches,
    pub github: ProviderSwitches,
    pub x: ProviderSwitches,
    pub mail_switches: MailSwitches,
    /// Requests per route.
    pub hits: BTreeMap<String, u64>,
}

/// What a call comes to. Signing needs the Worker's key, so a Google token
/// answer and the key set are finished by the Worker.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Reply(Reply),
    /// A Google token answer: `body` plus `id_token`, the RS256 JWT of
    /// `claims`, after `delay_ms`.
    IdToken { claims: Value, body: Value, delay_ms: u64 },
    /// The public key set.
    Jwks,
}

impl From<Reply> for Outcome {
    fn from(r: Reply) -> Self {
        Outcome::Reply(r)
    }
}

pub fn b64url(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

/// `base64url(header) . base64url(claims)`.
pub fn jwt_signing_input(header: &Value, claims: &Value) -> String {
    format!("{}.{}", b64url(header.to_string().as_bytes()), b64url(claims.to_string().as_bytes()))
}

/// The PKCE check (RFC 7636): the verifier's form, then S256 or plain.
pub fn pkce_ok(verifier: &str, challenge: &str, method: &str) -> bool {
    let form = (43..=128).contains(&verifier.len())
        && verifier.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    form && match method {
        "S256" => b64url(&Sha256::digest(verifier.as_bytes())) == challenge,
        "plain" => verifier == challenge,
        _ => false,
    }
}

/// The `http(s)` links in `text`.
pub fn links(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|w| w.starts_with("https://") || w.starts_with("http://"))
        .map(|w| w.trim_end_matches(['.', ',', ';', ':', ')', '"', '\'', '>']).to_owned())
        .collect()
}

fn error_page(status: u16, code: &str, detail: &str) -> Reply {
    Reply::html(
        status,
        &format!(
            "<!doctype html><title>Error {status}: {c}</title><h1>Error {status}: {c}</h1><p>{d}</p>",
            c = escape(code),
            d = escape(detail)
        ),
    )
}

fn oauth_error(status: u16, code: &str, detail: &str) -> Reply {
    Reply::json(status, &json!({"error": code, "error_description": detail}))
}

fn with_query(uri: &str, fields: &[(&str, &str)]) -> String {
    let sep = if uri.contains('?') { '&' } else { '?' };
    format!("{uri}{sep}{}", encode(fields))
}

/// The bearer token of a call (`Bearer <t>`, or GitHub's `token <t>`).
fn bearer(call: &Call) -> Option<&str> {
    let v = call.h("authorization")?.trim();
    v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")).or_else(|| v.strip_prefix("token ")).map(str::trim)
}

/// `(client_id, client_secret)` from a Basic header.
fn basic(call: &Call) -> Option<(String, String)> {
    let raw = call.h("authorization")?.trim().strip_prefix("Basic ")?;
    let dec = String::from_utf8(STANDARD.decode(raw.trim()).ok()?).ok()?;
    let (id, secret) = dec.split_once(':')?;
    Some((id.to_owned(), secret.to_owned()))
}

impl Stub {
    pub fn switches(&self, p: Provider) -> &ProviderSwitches {
        match p {
            Provider::Google => &self.google,
            Provider::Github => &self.github,
            Provider::X => &self.x,
        }
    }

    fn switches_mut(&mut self, p: Provider) -> &mut ProviderSwitches {
        match p {
            Provider::Google => &mut self.google,
            Provider::Github => &mut self.github,
            Provider::X => &mut self.x,
        }
    }

    fn fresh(&mut self, kind: &str, now_ms: u64) -> String {
        self.seq += 1;
        hex::encode(&Sha256::digest(format!("slop_ninja_dev_auth_stub/{kind}/{}/{now_ms}", self.seq).as_bytes())[..16])
    }

    fn hit(&mut self, route: &str) {
        *self.hits.entry(route.to_owned()).or_default() += 1;
    }

    fn prune(&mut self, now_ms: u64) {
        self.codes.retain(|c| now_ms.saturating_sub(c.issued_ms) <= 15 * 60_000);
        self.tokens.retain(|t| now_ms.saturating_sub(t.issued_ms) <= t.provider.token_ttl_ms());
    }

    /// Every route.
    pub fn handle(&mut self, cfg: &Config, call: &Call, now_ms: u64) -> Outcome {
        self.prune(now_ms);
        let m = call.method.as_str();
        let path = call.path.as_str();
        match (m, path) {
            ("GET", "/") => index(cfg).into(),
            ("GET", "/google/.well-known/openid-configuration") => {
                self.hit("google/discovery");
                Reply::json(200, &discovery(cfg)).into()
            }
            ("GET", "/google/oauth2/v3/certs") => {
                self.hit("google/jwks");
                Outcome::Jwks
            }
            ("GET", "/google/o/oauth2/v2/auth") => self.authorize(cfg, Provider::Google, call, now_ms, None).into(),
            ("GET", "/github/login/oauth/authorize") => self.authorize(cfg, Provider::Github, call, now_ms, None).into(),
            ("GET", "/x/i/oauth2/authorize") => self.authorize(cfg, Provider::X, call, now_ms, None).into(),
            ("GET", "/google/consent") => self.consent(cfg, Provider::Google, call, now_ms).into(),
            ("GET", "/github/consent") => self.consent(cfg, Provider::Github, call, now_ms).into(),
            ("GET", "/x/consent") => self.consent(cfg, Provider::X, call, now_ms).into(),
            ("POST", "/google/token") => self.google_token(cfg, call, now_ms),
            ("POST", "/github/login/oauth/access_token") => self.github_token(cfg, call, now_ms).into(),
            ("POST", "/x/2/oauth2/token") => self.x_token(cfg, call, now_ms).into(),
            ("POST", "/x/2/oauth2/revoke") => self.x_revoke(cfg, call).into(),
            ("GET", "/github/api/user") => self.github_user(call, now_ms, false).into(),
            ("GET", "/github/api/user/emails") => self.github_user(call, now_ms, true).into(),
            ("GET", "/x/2/users/me") => self.x_me(call, now_ms).into(),
            ("GET", p) if p.starts_with("/x/pbs/") => self.x_avatar(p).into(),
            ("POST", "/mail") => self.mail_post(call, now_ms).into(),
            ("GET", "/mail") => {
                let msgs: Vec<&Mail> = self.mail.iter().filter(|x| call.q("to").is_none_or(|to| x.to == to)).collect();
                Reply::json(200, &json!({"messages": msgs})).into()
            }
            ("GET", "/mail/latest") => match self.mail.iter().rev().find(|x| call.q("to").is_none_or(|to| x.to == to)) {
                Some(x) => Reply::json(200, x).into(),
                None => Reply::json(404, &json!({"error": "no mail"})).into(),
            },
            ("POST", "/mail/clear") | ("DELETE", "/mail") => {
                self.mail.clear();
                Reply::json(200, &json!({"cleared": true})).into()
            }
            ("GET", "/__stub/state") => Reply::json(200, &self.state_json()).into(),
            ("POST", "/__stub/switch") => self.switch(call).into(),
            ("POST", "/__stub/reset") => {
                *self = Stub { seq: self.seq, mail_seq: self.mail_seq, ..Stub::default() };
                Reply::json(200, &self.state_json()).into()
            }
            _ => Reply::json(404, &json!({"error": "not found"})).into(),
        }
    }

    pub fn state_json(&self) -> Value {
        json!({"switches": {"google": self.google, "github": self.github, "x": self.x, "mail": self.mail_switches},
            "hits": self.hits, "codes": self.codes.len(), "tokens": self.tokens.len(), "mail": self.mail.len()})
    }

    fn switch(&mut self, call: &Call) -> Reply {
        let Some(Value::Object(mut patch)) = call.json_body() else { return Reply::json(400, &json!({"error": "a JSON object"})) };
        let which = patch.remove("provider").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
        fn merge<T: Serialize + serde::de::DeserializeOwned>(cur: &T, patch: serde_json::Map<String, Value>) -> Result<T, String> {
            let mut v = serde_json::to_value(cur).map_err(|e| e.to_string())?;
            for (k, x) in patch {
                v[k] = x;
            }
            serde_json::from_value(v).map_err(|e| e.to_string())
        }
        let done = match which.as_str() {
            "google" | "github" | "x" => {
                let p = match which.as_str() {
                    "google" => Provider::Google,
                    "github" => Provider::Github,
                    _ => Provider::X,
                };
                merge(self.switches(p), patch).map(|s| *self.switches_mut(p) = s)
            }
            "mail" => merge(&self.mail_switches, patch).map(|s| self.mail_switches = s),
            _ => Err("provider: google, github, x or mail".to_owned()),
        };
        match done {
            Ok(()) => Reply::json(200, &self.state_json()),
            Err(e) => Reply::json(400, &json!({"error": e})),
        }
    }

    fn consent(&mut self, cfg: &Config, p: Provider, call: &Call, now_ms: u64) -> Reply {
        let chosen = call.q("user").unwrap_or("alice").to_owned();
        let allow = call.q("decision") != Some("deny");
        let mut inner = call.clone();
        inner.query.retain(|(k, _)| k != "user" && k != "decision");
        self.authorize(cfg, p, &inner, now_ms, Some((chosen, allow)))
    }

    fn authorize(&mut self, cfg: &Config, p: Provider, call: &Call, now_ms: u64, consent: Option<(String, bool)>) -> Reply {
        self.hit(&format!("{}/authorize", p.name()));
        let client = cfg.client(p);
        if call.q("client_id") != Some(client.id.as_str()) {
            return error_page(400, "invalid_client", "The OAuth client was not found.");
        }
        let Some(redirect) = call.q("redirect_uri").filter(|r| client.redirect_uris.iter().any(|u| u == r)) else {
            return error_page(400, "redirect_uri_mismatch", "The redirect_uri is not one registered for this client.");
        };
        let state = call.q("state").unwrap_or("");
        let back_error = |code: &str| Reply::redirect(302, &with_query(redirect, &[("error", code), ("state", state)]));
        if p != Provider::Github && call.q("response_type") != Some("code") {
            return back_error("unsupported_response_type");
        }
        if state.is_empty() || (p == Provider::X && state.len() > 500) {
            return error_page(400, "invalid_request", "state is missing or too long.");
        }
        let challenge = call.q("code_challenge").unwrap_or("");
        let method = call.q("code_challenge_method").unwrap_or("plain");
        let method_ok = match p {
            Provider::Github => method == "S256",
            _ => method == "S256" || method == "plain",
        };
        if challenge.is_empty() || !method_ok {
            return error_page(400, "invalid_request", "code_challenge is missing or its method is not supported.");
        }
        let scope = call.q("scope").unwrap_or("").to_owned();
        let has = |s: &str| scope.split_whitespace().any(|x| x == s);
        let scope_ok = match p {
            Provider::Google => has("openid"),
            Provider::Github => true,
            Provider::X => has("users.read") && has("tweet.read"),
        };
        if !scope_ok {
            return back_error("invalid_scope");
        }
        let sw = self.switches(p).clone();
        if consent.is_none() && sw.chooser {
            return chooser(p, call);
        }
        let (user_key, allow) = match consent {
            Some(c) => c,
            None => {
                let hinted = (p == Provider::Google).then(|| call.q("login_hint").and_then(users::by_hint)).flatten();
                let key = hinted.map(|u| u.key.to_owned()).or(sw.user.clone()).unwrap_or_else(|| "alice".to_owned());
                (key, !sw.deny)
            }
        };
        if !allow {
            let mut f = vec![("error", "access_denied"), ("state", state)];
            if p == Provider::Github {
                f.push(("error_description", "The user has denied your application access."));
            }
            return Reply::redirect(302, &with_query(redirect, &f));
        }
        if users::by_key(&user_key).is_none() {
            return error_page(400, "unknown_user", "No such test user.");
        }
        let code = match p {
            Provider::Google => format!("4/stub-{}", self.fresh("code", now_ms)),
            _ => self.fresh("code", now_ms),
        };
        self.codes.push(Code {
            code: code.clone(),
            provider: p,
            client_id: client.id.clone(),
            redirect_uri: redirect.to_owned(),
            challenge: challenge.to_owned(),
            method: method.to_owned(),
            scope: scope.clone(),
            nonce: call.q("nonce").map(str::to_owned),
            user: user_key,
            issued_ms: now_ms,
        });
        let mut f = vec![("code", code.as_str()), ("state", state)];
        if p == Provider::Google {
            f.extend([("scope", scope.as_str()), ("authuser", "0"), ("prompt", "consent")]);
        }
        Reply::redirect(302, &with_query(redirect, &f))
    }

    /// Takes the code a token request names, checking every binding; the
    /// error is `(status, code, detail)`.
    fn redeem(
        &mut self,
        p: Provider,
        client_id: &str,
        fields: &[(String, String)],
        now_ms: u64,
    ) -> Result<Code, (u16, &'static str, &'static str)> {
        let code = field(fields, "code").unwrap_or("");
        let Some(at) = self.codes.iter().position(|c| c.code == code && c.provider == p) else {
            return Err((400, "invalid_grant", "The code is unknown or was already used."));
        };
        let c = self.codes.remove(at);
        if now_ms.saturating_sub(c.issued_ms) > p.code_ttl_ms() {
            return Err((400, "invalid_grant", "The code expired."));
        }
        if c.client_id != client_id {
            return Err((401, "invalid_client", "The code was issued to another client."));
        }
        if field(fields, "redirect_uri") != Some(c.redirect_uri.as_str()) {
            return Err((400, "redirect_uri_mismatch", "The redirect_uri differs from the authorize request's."));
        }
        match field(fields, "code_verifier") {
            None => Err((400, "invalid_grant", "Missing code verifier.")),
            Some(v) if !pkce_ok(v, &c.challenge, &c.method) => Err((400, "invalid_grant", "Invalid code verifier.")),
            Some(_) => Ok(c),
        }
    }

    fn issue(&mut self, p: Provider, code: &Code, now_ms: u64) -> String {
        let t = match p {
            Provider::Google => format!("ya29.stub-{}", self.fresh("token", now_ms)),
            Provider::Github => format!("ghu_stub{}", self.fresh("token", now_ms)),
            Provider::X => format!("stub-{}", self.fresh("token", now_ms)),
        };
        self.tokens.push(Token { token: t.clone(), provider: p, user: code.user.clone(), scope: code.scope.clone(), issued_ms: now_ms });
        t
    }

    fn google_token(&mut self, cfg: &Config, call: &Call, now_ms: u64) -> Outcome {
        self.hit("google/token");
        let sw = self.google.clone();
        let delay = sw.token_delay_ms;
        let f = call.form_fields();
        let (id, secret) = basic(call)
            .unwrap_or_else(|| (field(&f, "client_id").unwrap_or("").to_owned(), field(&f, "client_secret").unwrap_or("").to_owned()));
        if id != cfg.google.id || secret != cfg.google.secret {
            return oauth_error(401, "invalid_client", "Unauthorized").with_delay(delay).into();
        }
        if field(&f, "grant_type") != Some("authorization_code") {
            return oauth_error(400, "unsupported_grant_type", "Invalid grant_type.").with_delay(delay).into();
        }
        let code = match self.redeem(Provider::Google, &id, &f, now_ms) {
            Ok(c) => c,
            Err((s, c, d)) => return oauth_error(s, c, d).with_delay(delay).into(),
        };
        if let Some(e) = &sw.token_error {
            return oauth_error(400, e, "stub switch").with_delay(delay).into();
        }
        let Some(who) = users::by_key(&code.user) else { return oauth_error(400, "invalid_grant", "unknown test user").into() };
        let access = self.issue(Provider::Google, &code, now_ms);
        let claims = google_claims(cfg, &sw, who, &code, now_ms);
        let body = json!({"access_token": access, "expires_in": 3599, "token_type": "Bearer",
            "scope": "openid https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile"});
        Outcome::IdToken { claims, body, delay_ms: delay }
    }

    fn github_token(&mut self, cfg: &Config, call: &Call, now_ms: u64) -> Reply {
        self.hit("github/token");
        let sw = self.github.clone();
        let json_out = call.h("accept").is_some_and(|a| a.contains("application/json"));
        let answer = |v: Value| {
            let r = if json_out {
                Reply::json(200, &v)
            } else {
                let pairs: Vec<(String, String)> = v
                    .as_object()
                    .map(|o| o.iter().map(|(k, x)| (k.clone(), x.as_str().map_or_else(|| x.to_string(), str::to_owned))).collect())
                    .unwrap_or_default();
                let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, x)| (k.as_str(), x.as_str())).collect();
                Reply::new(200).with_header("content-type", "application/x-www-form-urlencoded; charset=utf-8").with_body(encode(&refs).into_bytes())
            };
            r.with_delay(sw.token_delay_ms)
        };
        // GitHub answers a failed exchange with 200 and an `error` field.
        let gh_error = |code: &str, detail: &str| {
            answer(json!({"error": code, "error_description": detail,
                "error_uri": "https://docs.github.com/apps/managing-oauth-apps/troubleshooting-oauth-app-access-token-request-errors"}))
        };
        let f = call.form_fields();
        let id = field(&f, "client_id").unwrap_or("");
        if id != cfg.github.id || field(&f, "client_secret") != Some(cfg.github.secret.as_str()) {
            return gh_error("incorrect_client_credentials", "The client_id and/or client_secret passed are incorrect.");
        }
        let id = id.to_owned();
        let code = match self.redeem(Provider::Github, &id, &f, now_ms) {
            Ok(c) => c,
            Err((_, "redirect_uri_mismatch", d)) => return gh_error("redirect_uri_mismatch", d),
            Err((_, _, d)) => return gh_error("bad_verification_code", d),
        };
        if let Some(e) = &sw.token_error {
            return gh_error(e, "stub switch");
        }
        let t = self.issue(Provider::Github, &code, now_ms);
        answer(json!({"access_token": t, "token_type": "bearer", "scope": ""}))
    }

    fn x_token(&mut self, cfg: &Config, call: &Call, now_ms: u64) -> Reply {
        self.hit("x/token");
        let delay = self.x.token_delay_ms;
        let token_error = self.x.token_error.clone();
        let Some((id, _)) = basic(call).filter(|(i, s)| *i == cfg.x.id && *s == cfg.x.secret) else {
            return oauth_error(401, "unauthorized_client", "Missing valid authorization header").with_delay(delay);
        };
        let f = call.form_fields();
        if field(&f, "grant_type") != Some("authorization_code") {
            return oauth_error(400, "invalid_request", "Missing required parameter [grant_type].").with_delay(delay);
        }
        let code = match self.redeem(Provider::X, &id, &f, now_ms) {
            Ok(c) => c,
            Err((s, _, _)) => {
                return oauth_error(s.min(400), "invalid_request", "Value passed for the authorization code was invalid.").with_delay(delay);
            }
        };
        if let Some(e) = token_error {
            return oauth_error(400, &e, "stub switch").with_delay(delay);
        }
        let t = self.issue(Provider::X, &code, now_ms);
        Reply::json(200, &json!({"token_type": "bearer", "expires_in": 7200, "access_token": t, "scope": code.scope})).with_delay(delay)
    }

    fn x_revoke(&mut self, cfg: &Config, call: &Call) -> Reply {
        self.hit("x/revoke");
        if basic(call).is_none_or(|(i, s)| i != cfg.x.id || s != cfg.x.secret) {
            return oauth_error(401, "unauthorized_client", "Missing valid authorization header");
        }
        let f = call.form_fields();
        let t = field(&f, "token").unwrap_or("");
        self.tokens.retain(|x| !(x.provider == Provider::X && x.token == t));
        Reply::json(200, &json!({"revoked": true}))
    }

    fn token_user(&self, p: Provider, call: &Call, now_ms: u64) -> Option<(&'static User, String)> {
        let t = bearer(call)?;
        let tok = self.tokens.iter().find(|x| x.provider == p && x.token == t && now_ms.saturating_sub(x.issued_ms) <= p.token_ttl_ms())?;
        Some((users::by_key(&tok.user)?, tok.scope.clone()))
    }

    fn github_user(&mut self, call: &Call, now_ms: u64, emails: bool) -> Reply {
        self.hit(if emails { "github/user/emails" } else { "github/user" });
        let sw = self.github.clone();
        if call.h("user-agent").is_none_or(|u| u.trim().is_empty()) {
            return Reply::json(403, &json!({"message": "Request forbidden by administrative rules. Please make sure your request has a User-Agent header (https://docs.github.com/en/rest/overview/resources-in-the-rest-api#user-agent-required). Check https://developer.github.com for other possible causes."}));
        }
        if let Some(s) = sw.userinfo_status {
            return Reply::json(s, &json!({"message": "stub switch", "status": s.to_string()}));
        }
        let Some((u, _)) = self.token_user(Provider::Github, call, now_ms) else {
            return Reply::json(401, &json!({"message": "Bad credentials", "documentation_url": "https://docs.github.com/rest", "status": "401"}));
        };
        if !emails {
            return Reply::json(200, &json!({
                "login": u.github_login, "id": u.github_id, "node_id": format!("U_stub{}", u.github_id),
                "avatar_url": format!("https://avatars.githubusercontent.com/u/{}?v=4", u.github_id), "gravatar_id": "",
                "html_url": format!("https://github.com/{}", u.github_login), "type": "User", "site_admin": false,
                "name": u.name, "company": null, "blog": "", "location": null, "email": null, "hireable": null, "bio": null,
                "public_repos": 0, "followers": 0, "following": 0,
                "created_at": "2020-01-01T00:00:00Z", "updated_at": "2020-01-01T00:00:00Z",
            }));
        }
        let placeholder = format!("{}+{}@users.noreply.github.com", u.github_id, u.github_login);
        let list = match (sw.email, u.email) {
            (Some(EmailMode::Missing), _) | (None | Some(EmailMode::Unverified), None) => json!([]),
            (Some(EmailMode::Placeholder), _) => json!([{"email": placeholder, "primary": true, "verified": true, "visibility": "private"}]),
            (mode, Some(addr)) => {
                let verified = u.email_verified && mode != Some(EmailMode::Unverified);
                // A verified address that is not primary comes first, so a
                // client must pick the primary one, not the first.
                json!([
                    {"email": format!("{}.alt@example.org", u.key), "primary": false, "verified": true, "visibility": null},
                    {"email": addr, "primary": true, "verified": verified, "visibility": "private"},
                ])
            }
        };
        Reply::json(200, &list)
    }

    fn x_me(&mut self, call: &Call, now_ms: u64) -> Reply {
        self.hit("x/users/me");
        let sw = self.x.clone();
        if let Some(s) = sw.userinfo_status {
            let body = match s {
                402 => json!({"title": "CreditsDepleted", "detail": "Your enrolled account does not have any credits to fulfill this request.", "type": "https://api.twitter.com/2/problems/credits", "status": 402}),
                429 => json!({"title": "Too Many Requests", "detail": "Too Many Requests", "type": "about:blank", "status": 429}),
                _ => json!({"title": "Stub switch", "detail": "stub switch", "type": "about:blank", "status": s}),
            };
            return Reply::json(s, &body);
        }
        let Some((u, scope)) = self.token_user(Provider::X, call, now_ms) else {
            return Reply::json(401, &json!({"title": "Unauthorized", "type": "about:blank", "status": 401, "detail": "Unauthorized"}));
        };
        let has = |s: &str| scope.split_whitespace().any(|x| x == s);
        if !(has("users.read") && has("tweet.read")) {
            return Reply::json(403, &json!({"title": "Forbidden", "type": "about:blank", "status": 403, "detail": "Forbidden"}));
        }
        let fields: Vec<&str> = call.q("user.fields").map(|f| f.split(',').map(str::trim).collect()).unwrap_or_default();
        let mut data = json!({"id": u.x_id, "name": u.name, "username": u.x_username});
        for f in fields {
            match f {
                "confirmed_email" => {
                    let shown = has("users.email") && u.email_verified && sw.email.is_none();
                    if let (true, Some(addr)) = (shown, u.email) {
                        data["confirmed_email"] = json!(addr);
                    }
                }
                "profile_image_url" => {
                    data["profile_image_url"] = json!(format!("https://pbs.twimg.com/profile_images/{}/{}_normal.png", u.x_id, u.key));
                }
                "verified_type" => data["verified_type"] = json!("none"),
                "created_at" => data["created_at"] = json!("2020-01-01T00:00:00.000Z"),
                _ => {}
            }
        }
        Reply::json(200, &json!({"data": data}))
    }

    /// `/x/pbs/profile_images/<id>/<key>_normal.<ext>`: what the account
    /// Worker's dev build fetches for `https://pbs.twimg.com/...`.
    fn x_avatar(&mut self, path: &str) -> Reply {
        self.hit("x/avatar");
        let file = path.rsplit('/').next().unwrap_or("");
        let key = file.split('_').next().unwrap_or("");
        let Some(u) = users::by_key(key) else { return Reply::new(404) };
        let kind = self.x.avatar.unwrap_or(u.avatar);
        match (kind, users::avatar_bytes(kind)) {
            (Avatar::Redirect, _) => Reply::redirect(302, &format!("https://pbs.twimg.com/profile_images/{}/other_normal.png", u.x_id)),
            (_, Some((bytes, ty))) => Reply::new(200).with_header("content-type", ty).with_body(bytes),
            (_, None) => Reply::new(404),
        }
    }

    fn mail_post(&mut self, call: &Call, now_ms: u64) -> Reply {
        self.hit("mail");
        let sw = self.mail_switches.clone();
        let Some(v) = call.json_body() else { return Reply::json(400, &json!({"error": "a JSON body"})) };
        let s = |k: &str| v[k].as_str().map(str::to_owned);
        let (Some(to), Some(subject), Some(text)) = (s("to"), s("subject"), s("text")) else {
            return Reply::json(400, &json!({"error": "to, subject and text are required"}));
        };
        if let Some(e) = sw.error {
            return Reply::json(422, &json!({"error": e})).with_delay(sw.delay_ms);
        }
        if sw.suppressed.contains(&to) {
            return Reply::json(422, &json!({"error": "E_RECIPIENT_SUPPRESSED"})).with_delay(sw.delay_ms);
        }
        self.mail_seq += 1;
        let links = links(&text);
        self.mail.push(Mail {
            id: self.mail_seq,
            received_ms: now_ms,
            to,
            from: s("from"),
            subject,
            text,
            html: s("html"),
            headers: v.get("headers").cloned().unwrap_or(Value::Null),
            links,
        });
        if self.mail.len() > MAX_MAIL {
            let over = self.mail.len() - MAX_MAIL;
            self.mail.drain(..over);
        }
        Reply::json(201, &json!({"id": self.mail_seq})).with_delay(sw.delay_ms)
    }
}

fn google_claims(cfg: &Config, sw: &ProviderSwitches, u: &User, code: &Code, now_ms: u64) -> Value {
    let iat = now_ms / 1000;
    let fault = sw.id_token;
    let iss = match fault {
        Some(IdTokenFault::IssBare) => "accounts.google.com",
        Some(IdTokenFault::IssWrong) => "https://accounts.example.com",
        _ => "https://accounts.google.com",
    };
    let aud = if fault == Some(IdTokenFault::AudWrong) { "another-client.apps.example.com".to_owned() } else { cfg.google.id.clone() };
    let (iat, exp) = if fault == Some(IdTokenFault::Expired) { (iat.saturating_sub(7200), iat.saturating_sub(3600)) } else { (iat, iat + 3600) };
    let mut c = json!({"iss": iss, "azp": cfg.google.id, "aud": aud, "sub": u.google_sub, "name": u.name, "iat": iat, "exp": exp});
    match (sw.email, u.email) {
        (Some(EmailMode::Missing), _) | (_, None) => {}
        (mode, Some(addr)) => {
            c["email"] = json!(addr);
            c["email_verified"] = json!(u.email_verified && mode != Some(EmailMode::Unverified));
        }
    }
    match (fault, &code.nonce) {
        (Some(IdTokenFault::NonceMissing), _) => {}
        (Some(IdTokenFault::NonceWrong), _) => c["nonce"] = json!("not-the-nonce"),
        (_, Some(n)) => c["nonce"] = json!(n),
        (_, None) => {}
    }
    c
}

/// Google's discovery document, with the stub's endpoints.
pub fn discovery(cfg: &Config) -> Value {
    let o = &cfg.origin;
    json!({
        "issuer": "https://accounts.google.com",
        "authorization_endpoint": format!("{o}/google/o/oauth2/v2/auth"),
        "token_endpoint": format!("{o}/google/token"),
        "jwks_uri": format!("{o}/google/oauth2/v3/certs"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid", "email", "profile"],
        "token_endpoint_auth_methods_supported": ["client_secret_post", "client_secret_basic"],
        "claims_supported": ["aud", "email", "email_verified", "exp", "iat", "iss", "name", "sub"],
        "code_challenge_methods_supported": ["plain", "S256"],
        "grant_types_supported": ["authorization_code"],
    })
}

fn chooser(p: Provider, call: &Call) -> Reply {
    let hidden: String = call
        .query
        .iter()
        .map(|(k, v)| format!("<input type=\"hidden\" name=\"{}\" value=\"{}\">", escape(k), escape(v)))
        .collect();
    let buttons: String = users::USERS
        .iter()
        .map(|u| format!("<button name=\"user\" value=\"{k}\">{k} ({e})</button> ", k = u.key, e = escape(u.email.unwrap_or("no email"))))
        .collect();
    Reply::html(
        200,
        &format!(
            "<!doctype html><title>{p} sign-in (stub)</title><h1>{p} sign-in (dev stub)</h1>\
             <form method=\"get\" action=\"/{p}/consent\">{hidden}{buttons}\
             <button name=\"decision\" value=\"deny\">Deny</button></form>",
            p = p.name()
        ),
    )
}

fn index(cfg: &Config) -> Reply {
    let users: String = users::USERS
        .iter()
        .map(|u| format!("<li>{} &lt;{}&gt;</li>", escape(u.key), escape(u.email.unwrap_or("no email"))))
        .collect();
    Reply::html(
        200,
        &format!(
            "<!doctype html><title>auth stub</title><h1>SlopNinja dev auth stub</h1>\
             <p>Origin {}. Dev only: OAuth for Google, GitHub and X, and the mail capture at /mail.</p><ul>{users}</ul>",
            escape(&cfg.origin)
        ),
    )
}

#[cfg(test)]
mod tests;

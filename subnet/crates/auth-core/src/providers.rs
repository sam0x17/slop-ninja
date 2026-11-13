//! Google, GitHub and X: endpoint and parameter tables, request builders and
//! bounded response parsers.
//!
//! Every endpoint is a constant. The account Worker makes no discovery fetch
//! and needs no cache: Google's ID token comes straight from its token
//! endpoint over TLS with the client secret, so no JWKS is fetched either,
//! and a drill compares [`GOOGLE_DISCOVERY`] with these constants once.
//! Provider-URL overrides for the local auth stub exist only under the `dev`
//! feature ([`Spec::endpoints_at`]).
//!
//! Each parser reads a body only after checking its length against the
//! bound named beside it, keeps only the fields sign-in needs, and checks
//! each of those against its format. Provider tokens are used once and
//! dropped: nothing here stores one, and no refresh token is requested.

use std::borrow::Cow;

use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;

use crate::email_addr::{self, EmailAddr};
use crate::origin::Origin;

/// A sign-in provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Google (OpenID Connect).
    Google,
    /// GitHub (a GitHub App used for sign-in).
    GitHub,
    /// X (OAuth 2.0 with PKCE).
    X,
}

impl Provider {
    /// Every provider, in button order.
    pub const ALL: [Self; 3] = [Self::Google, Self::GitHub, Self::X];

    /// The path segment and stored name: `google`, `github` or `x`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::GitHub => "github",
            Self::X => "x",
        }
    }

    /// The name shown to readers and in mail.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Google => "Google",
            Self::GitHub => "GitHub",
            Self::X => "X",
        }
    }

    /// Parses a stored or path name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == name)
    }

    /// The provider's table.
    #[must_use]
    pub const fn spec(self) -> &'static Spec {
        match self {
            Self::Google => &GOOGLE,
            Self::GitHub => &GITHUB,
            Self::X => &X,
        }
    }

    /// The callback path, one per provider (a mix-up defence).
    #[must_use]
    pub fn callback_path(self) -> String {
        format!("/auth/{}/callback", self.as_str())
    }

    /// The exact `redirect_uri` registered with the provider, from the
    /// configured site origin.
    #[must_use]
    pub fn redirect_uri(self, site: &Origin) -> String {
        format!("{}{}", site.as_str(), self.callback_path())
    }
}

/// How the token exchange authenticates the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientAuth {
    /// `client_id` and `client_secret` in the form body (Google, GitHub).
    Body,
    /// `Authorization: Basic base64(client_id:client_secret)` (X).
    Basic,
}

/// One provider's table.
#[derive(Debug)]
pub struct Spec {
    /// The provider.
    pub provider: Provider,
    /// Authorization endpoint (a browser redirect).
    pub authorize: &'static str,
    /// Token endpoint (server to server).
    pub token: &'static str,
    /// The profile read after the exchange, if the provider needs one.
    pub user: Option<&'static str>,
    /// GitHub's address list.
    pub emails: Option<&'static str>,
    /// The `scope` parameter, if any (a GitHub App has none: its
    /// permissions are set on the app).
    pub scope: Option<&'static str>,
    /// Fixed extra authorize parameters.
    pub extra_authorize: &'static [(&'static str, &'static str)],
    /// Whether the authorize request carries an OIDC `nonce`.
    pub sends_nonce: bool,
    /// How the exchange authenticates.
    pub client_auth: ClientAuth,
    /// Whether the token request carries `grant_type=authorization_code`.
    pub sends_grant_type: bool,
}

/// Google's discovery document, read only by the drill that checks
/// [`GOOGLE`]'s constants against it; the Worker never fetches it.
pub const GOOGLE_DISCOVERY: &str = "https://accounts.google.com/.well-known/openid-configuration";

/// The two forms of Google's ID token `iss`.
pub const GOOGLE_ISSUERS: [&str; 2] = ["https://accounts.google.com", "accounts.google.com"];

/// Google: OIDC with `openid email profile` (`profile` only for the
/// brand-verified consent screen; no profile field is stored), a nonce and
/// the account chooser. No `access_type=offline`.
pub const GOOGLE: Spec = Spec {
    provider: Provider::Google,
    authorize: "https://accounts.google.com/o/oauth2/v2/auth",
    token: "https://oauth2.googleapis.com/token",
    user: None,
    emails: None,
    scope: Some("openid email profile"),
    extra_authorize: &[("prompt", "select_account")],
    sends_nonce: true,
    client_auth: ClientAuth::Body,
    sends_grant_type: true,
};

/// GitHub: a GitHub App with one account permission (email addresses,
/// read), so no `scope`; `/user` for the numeric id and login, and
/// `/user/emails` for the primary verified address.
pub const GITHUB: Spec = Spec {
    provider: Provider::GitHub,
    authorize: "https://github.com/login/oauth/authorize",
    token: "https://github.com/login/oauth/access_token",
    user: Some("https://api.github.com/user"),
    emails: Some("https://api.github.com/user/emails"),
    scope: None,
    extra_authorize: &[("prompt", "select_account")],
    sends_nonce: false,
    client_auth: ClientAuth::Body,
    sends_grant_type: false,
};

/// X: OAuth 2.0 with PKCE, `users.read tweet.read users.email` (no
/// `offline.access`), Basic client authentication, and one profile read.
pub const X: Spec = Spec {
    provider: Provider::X,
    authorize: "https://x.com/i/oauth2/authorize",
    token: "https://api.x.com/2/oauth2/token",
    user: Some(
        "https://api.x.com/2/users/me?user.fields=confirmed_email,profile_image_url,username,name,verified_type",
    ),
    emails: None,
    scope: Some("users.read tweet.read users.email"),
    extra_authorize: &[],
    sends_nonce: false,
    client_auth: ClientAuth::Basic,
    sends_grant_type: true,
};

/// The `User-Agent` sent on server-to-server calls (GitHub's API refuses
/// requests without one).
pub const USER_AGENT: &str = "SlopNinja-sign-in";

/// A provider's resolved endpoints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    /// Authorization endpoint.
    pub authorize: Cow<'static, str>,
    /// Token endpoint.
    pub token: Cow<'static, str>,
    /// Profile endpoint.
    pub user: Option<Cow<'static, str>>,
    /// Address-list endpoint.
    pub emails: Option<Cow<'static, str>>,
}

impl Spec {
    /// The production endpoints: the constants.
    #[must_use]
    pub fn endpoints(&self) -> Endpoints {
        Endpoints {
            authorize: Cow::Borrowed(self.authorize),
            token: Cow::Borrowed(self.token),
            user: self.user.map(Cow::Borrowed),
            emails: self.emails.map(Cow::Borrowed),
        }
    }

    /// Local development only: every endpoint moved under the auth stub's
    /// origin as `<base>/<real host><real path and query>`, for example
    /// `http://127.0.0.1:8790/oauth2.googleapis.com/token`, so the stub
    /// routes on the real host and path.
    #[cfg(feature = "dev")]
    #[must_use]
    pub fn endpoints_at(&self, base: &Origin) -> Endpoints {
        let move_url = |u: &'static str| -> Cow<'static, str> {
            let rest = u.strip_prefix("https://").unwrap_or(u);
            Cow::Owned(format!("{}/{rest}", base.as_str()))
        };
        Endpoints {
            authorize: move_url(self.authorize),
            token: move_url(self.token),
            user: self.user.map(move_url),
            emails: self.emails.map(move_url),
        }
    }
}

/// RFC 3986 unreserved characters stay; everything else is percent-encoded
/// (a space becomes `%20`, never `+`).
const QUERY_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// What an authorize redirect carries besides the table's constants.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizeParams<'a> {
    /// The provider's client id (config).
    pub client_id: &'a str,
    /// [`Provider::redirect_uri`].
    pub redirect_uri: &'a str,
    /// The flow's `state` token text.
    pub state: &'a str,
    /// The OIDC nonce token text (Google only; ignored elsewhere).
    pub nonce: &'a str,
    /// [`crate::pkce::Verifier::challenge`].
    pub code_challenge: &'a str,
}

/// The authorize redirect's `Location`.
#[must_use]
pub fn authorize_url(spec: &Spec, endpoints: &Endpoints, p: &AuthorizeParams<'_>) -> String {
    let mut pairs: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", p.client_id),
        ("redirect_uri", p.redirect_uri),
    ];
    if let Some(scope) = spec.scope {
        pairs.push(("scope", scope));
    }
    pairs.push(("state", p.state));
    if spec.sends_nonce {
        pairs.push(("nonce", p.nonce));
    }
    pairs.push(("code_challenge", p.code_challenge));
    pairs.push(("code_challenge_method", crate::pkce::METHOD));
    pairs.extend_from_slice(spec.extra_authorize);
    let mut url = endpoints.authorize.clone().into_owned();
    let mut sep = if url.contains('?') { '&' } else { '?' };
    for (k, v) in pairs {
        url.push(sep);
        url.push_str(k);
        url.push('=');
        url.extend(utf8_percent_encode(v, QUERY_VALUE));
        sep = '&';
    }
    url
}

/// A server-to-server request for the Worker to send.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The URL.
    pub url: String,
    /// Headers, in order.
    pub headers: Vec<(&'static str, String)>,
    /// A form-encoded body for `POST`, empty for `GET`.
    pub body: String,
}

impl core::fmt::Debug for HttpRequest {
    // Bodies and headers carry the client secret, the code, the verifier or
    // a bearer token: never print them.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// What the token exchange carries.
#[derive(Clone, Copy)]
pub struct ExchangeParams<'a> {
    /// The client id (config).
    pub client_id: &'a str,
    /// The client secret (a Worker secret).
    pub client_secret: &'a str,
    /// The callback's `code`.
    pub code: &'a str,
    /// The same `redirect_uri` the authorize request sent.
    pub redirect_uri: &'a str,
    /// The flow's PKCE verifier.
    pub code_verifier: &'a str,
}

/// The token exchange request.
#[must_use]
pub fn token_request(spec: &Spec, endpoints: &Endpoints, p: &ExchangeParams<'_>) -> HttpRequest {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    if spec.sends_grant_type {
        form.append_pair("grant_type", "authorization_code");
    }
    form.append_pair("code", p.code);
    form.append_pair("redirect_uri", p.redirect_uri);
    form.append_pair("code_verifier", p.code_verifier);
    let mut headers = vec![
        (
            "Content-Type",
            "application/x-www-form-urlencoded".to_owned(),
        ),
        ("Accept", "application/json".to_owned()),
        ("User-Agent", USER_AGENT.to_owned()),
    ];
    match spec.client_auth {
        ClientAuth::Body => {
            form.append_pair("client_id", p.client_id);
            form.append_pair("client_secret", p.client_secret);
        }
        ClientAuth::Basic => {
            // X documents `base64(client_id:client_secret)` without the form
            // encoding of RFC 6749 section 2.3.1; its ids and secrets use
            // only characters that encoding would leave unchanged.
            let pair = format!("{}:{}", p.client_id, p.client_secret);
            let b64 = base64::engine::general_purpose::STANDARD.encode(pair);
            headers.push(("Authorization", format!("Basic {b64}")));
        }
    }
    HttpRequest {
        method: "POST",
        url: endpoints.token.clone().into_owned(),
        headers,
        body: form.finish(),
    }
}

/// A bearer-authenticated `GET` of a profile endpoint.
#[must_use]
pub fn api_get(spec: &Spec, url: &str, access_token: &str) -> HttpRequest {
    let mut headers = vec![
        ("Authorization", format!("Bearer {access_token}")),
        ("User-Agent", USER_AGENT.to_owned()),
    ];
    if spec.provider == Provider::GitHub {
        headers.push(("Accept", "application/vnd.github+json".to_owned()));
        headers.push(("X-GitHub-Api-Version", "2022-11-28".to_owned()));
    } else {
        headers.push(("Accept", "application/json".to_owned()));
    }
    HttpRequest {
        method: "GET",
        url: url.to_owned(),
        headers,
        body: String::new(),
    }
}

/// Why a provider response was refused. Content-free: no provider text is
/// kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// The body is longer than its bound.
    TooLarge,
    /// The body does not parse, or a needed field is missing or out of
    /// format.
    Malformed,
    /// The provider refused the request (an OAuth `error`, or 400/401/403).
    Refused,
    /// The provider is refusing for now (402, 429 or 503, such as a rate
    /// limit); the button reads "unavailable right now" and other methods
    /// keep working.
    Unavailable,
    /// Any other non-success status.
    Upstream,
}

impl ProviderError {
    /// The journal reason string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
            Self::Refused => "refused",
            Self::Unavailable => "unavailable",
            Self::Upstream => "upstream",
        }
    }
}

fn status_error(status: u16) -> Option<ProviderError> {
    match status {
        200..=299 => None,
        402 | 429 | 503 => Some(ProviderError::Unavailable),
        400 | 401 | 403 => Some(ProviderError::Refused),
        _ => Some(ProviderError::Upstream),
    }
}

fn bounded(body: &[u8], max: usize) -> Result<&[u8], ProviderError> {
    if body.len() > max {
        Err(ProviderError::TooLarge)
    } else {
        Ok(body)
    }
}

/// The longest token response read (Google's, with its ID token, is about
/// 2 KiB).
pub const TOKEN_BODY_MAX: usize = 16 * 1024;
/// The longest access token accepted (the three providers issue well under
/// 300 characters).
pub const ACCESS_TOKEN_MAX: usize = 4096;
/// The longest ID token accepted.
pub const ID_TOKEN_MAX: usize = 8192;
/// The longest profile or address-list body read.
pub const PROFILE_BODY_MAX: usize = 64 * 1024;

/// A successful token exchange. The access token is used once and dropped.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenResponse {
    /// The access token.
    pub access_token: String,
    /// Google's ID token.
    pub id_token: Option<String>,
}

impl core::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("TokenResponse(..)")
    }
}

#[derive(Deserialize)]
struct RawToken {
    access_token: Option<String>,
    token_type: Option<String>,
    id_token: Option<String>,
    error: Option<String>,
}

/// Parses a token endpoint's answer. GitHub reports errors in a 200 body,
/// so an `error` field refuses whatever the status.
pub fn parse_token_response(
    spec: &Spec,
    status: u16,
    body: &[u8],
) -> Result<TokenResponse, ProviderError> {
    let body = bounded(body, TOKEN_BODY_MAX)?;
    let parsed: Option<RawToken> = serde_json::from_slice(body).ok();
    if parsed.as_ref().is_some_and(|r| r.error.is_some()) {
        return Err(status_error(status).unwrap_or(ProviderError::Refused));
    }
    if let Some(e) = status_error(status) {
        return Err(e);
    }
    let raw = parsed.ok_or(ProviderError::Malformed)?;
    let access_token = raw.access_token.ok_or(ProviderError::Malformed)?;
    if access_token.is_empty()
        || access_token.len() > ACCESS_TOKEN_MAX
        || !access_token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ProviderError::Malformed);
    }
    if let Some(t) = &raw.token_type
        && !t.eq_ignore_ascii_case("bearer")
    {
        return Err(ProviderError::Malformed);
    }
    let id_token = match (spec.sends_nonce, raw.id_token) {
        (true, Some(t)) if !t.is_empty() && t.len() <= ID_TOKEN_MAX => Some(t),
        (true, _) => return Err(ProviderError::Malformed),
        (false, _) => None,
    };
    Ok(TokenResponse {
        access_token,
        id_token,
    })
}

/// A GitHub account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubUser {
    /// The numeric id: the identity key.
    pub id: u64,
    /// The login: the display handle only.
    pub login: String,
}

#[derive(Deserialize)]
struct RawGitHubUser {
    id: Option<u64>,
    login: Option<String>,
}

/// Parses `GET /user`.
pub fn parse_github_user(status: u16, body: &[u8]) -> Result<GitHubUser, ProviderError> {
    let body = bounded(body, PROFILE_BODY_MAX)?;
    if let Some(e) = status_error(status) {
        return Err(e);
    }
    let raw: RawGitHubUser = serde_json::from_slice(body).map_err(|_| ProviderError::Malformed)?;
    let id = raw.id.filter(|&i| i > 0).ok_or(ProviderError::Malformed)?;
    let login = raw.login.ok_or(ProviderError::Malformed)?;
    let login_ok = (1..=39).contains(&login.len())
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !login_ok {
        return Err(ProviderError::Malformed);
    }
    Ok(GitHubUser { id, login })
}

#[derive(Deserialize)]
struct RawGitHubEmail {
    email: Option<String>,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    verified: bool,
}

/// Parses `GET /user/emails` and returns the primary verified address, if
/// it is one mail can reach.
///
/// GitHub's `noreply` addresses (and the placeholder an enterprise-managed
/// account gets for an unverified address) count as unverified; an address
/// [`email_addr::parse`] refuses counts as none.
pub fn parse_github_emails(status: u16, body: &[u8]) -> Result<Option<EmailAddr>, ProviderError> {
    let body = bounded(body, PROFILE_BODY_MAX)?;
    if let Some(e) = status_error(status) {
        return Err(e);
    }
    let list: Vec<RawGitHubEmail> =
        serde_json::from_slice(body).map_err(|_| ProviderError::Malformed)?;
    let Some(primary) = list.into_iter().find(|e| e.primary && e.verified) else {
        return Ok(None);
    };
    let Some(addr) = primary.email.and_then(|e| email_addr::parse(&e).ok()) else {
        return Ok(None);
    };
    let domain = addr.domain();
    if domain == "noreply.github.com" || domain.ends_with(".noreply.github.com") {
        return Ok(None);
    }
    Ok(Some(addr))
}

/// An X account, as `GET /2/users/me` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XUser {
    /// The numeric user id (decimal text): the identity key.
    pub id: String,
    /// The @handle without the `@`, refreshed at each sign-in.
    pub username: String,
    /// The display name, when present and printable.
    pub name: Option<String>,
    /// The `_normal` avatar URL, only when it passes [`x_avatar_url`].
    pub profile_image_url: Option<String>,
    /// The confirmed address, when X returns one that parses. An X identity
    /// never links by email, so its absence changes nothing.
    pub confirmed_email: Option<EmailAddr>,
    /// `blue`, `business`, `government` or `none`, when X returns one of
    /// them.
    pub verified_type: Option<String>,
}

#[derive(Deserialize)]
struct RawXEnvelope {
    data: Option<RawXUser>,
}

#[derive(Deserialize)]
struct RawXUser {
    id: Option<String>,
    username: Option<String>,
    name: Option<String>,
    profile_image_url: Option<String>,
    confirmed_email: Option<String>,
    verified_type: Option<String>,
}

/// The longest display name kept (X allows 50 characters).
pub const X_NAME_MAX_CHARS: usize = 50;

/// Parses `GET /2/users/me`.
pub fn parse_x_me(status: u16, body: &[u8]) -> Result<XUser, ProviderError> {
    let body = bounded(body, PROFILE_BODY_MAX)?;
    if let Some(e) = status_error(status) {
        return Err(e);
    }
    let env: RawXEnvelope = serde_json::from_slice(body).map_err(|_| ProviderError::Malformed)?;
    let raw = env.data.ok_or(ProviderError::Malformed)?;
    let id = raw.id.ok_or(ProviderError::Malformed)?;
    if !(1..=20).contains(&id.len())
        || !id.bytes().all(|b| b.is_ascii_digit())
        || id.starts_with('0')
    {
        return Err(ProviderError::Malformed);
    }
    let username = raw.username.ok_or(ProviderError::Malformed)?;
    if !(1..=15).contains(&username.len())
        || !username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(ProviderError::Malformed);
    }
    let name = raw
        .name
        .filter(|n| !n.is_empty() && n.chars().count() <= X_NAME_MAX_CHARS)
        .filter(|n| !n.chars().any(char::is_control));
    let profile_image_url = raw.profile_image_url.filter(|u| x_avatar_url(u).is_some());
    let confirmed_email = raw.confirmed_email.and_then(|e| email_addr::parse(&e).ok());
    let verified_type = raw
        .verified_type
        .filter(|v| matches!(v.as_str(), "blue" | "business" | "government" | "none"));
    Ok(XUser {
        id,
        username,
        name,
        profile_image_url,
        confirmed_email,
        verified_type,
    })
}

/// The largest avatar body stored; a longer body stores nothing.
pub const AVATAR_MAX: usize = 64 * 1024;

/// The only avatar host fetched.
pub const X_AVATAR_HOST: &str = "pbs.twimg.com";

/// Checks an X `profile_image_url`: it is fetched only when it is exactly
/// `https://pbs.twimg.com/…` (scheme `https`, that host, the default port,
/// no user info). Returns the URL to fetch, without following redirects.
#[must_use]
pub fn x_avatar_url(raw: &str) -> Option<url::Url> {
    let url = url::Url::parse(raw).ok()?;
    let ok = url.scheme() == "https"
        && url.host_str() == Some(X_AVATAR_HOST)
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none();
    ok.then_some(url)
}

/// An avatar's type, decided from its bytes, never from the upstream
/// `Content-Type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageType {
    /// `FF D8 FF`.
    Jpeg,
    /// `89 50 4E 47 0D 0A 1A 0A`.
    Png,
    /// `GIF87a` or `GIF89a`.
    Gif,
    /// `RIFF`, four bytes, `WEBP`.
    Webp,
}

impl ImageType {
    /// Sniffs the four accepted types; anything else (SVG, HTML, unknown)
    /// is `None`, and a body over [`AVATAR_MAX`] is `None` too.
    #[must_use]
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > AVATAR_MAX {
            return None;
        }
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(Self::Png)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }

    /// The `Content-Type` the avatar route serves.
    #[must_use]
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

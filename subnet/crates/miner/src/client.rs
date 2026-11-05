//! Client of the admission API (`docs/testnet/admission-api.md`).
//!
//! Every request except the cohort read is signed by the hotkey over
//! `slop_ninja_admission_wire::request_message`. Plain HTTP is accepted only
//! for loopback hosts; everything else must be HTTPS with the system's trust
//! roots and an explicit ring provider. Redirects are never followed, so the
//! signed headers go only to the configured origin. No request to the
//! coordinator carries an artifact byte: the largest body is one
//! `SubmissionUpload` record, and the header and files go straight to R2
//! under the presigned URLs these requests return ([`crate::direct`]).
//! Response bodies are read up to the contract's
//! [`response_limit`](slop_ninja_admission_wire::response_limit).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use futures::future::BoxFuture;
use reqwest::{Method, StatusCode};
use slop_ninja_admission_wire::json::{
    CohortInfo, ErrorBody, HotkeyView, SCHEMA_COHORT, SCHEMA_HOTKEY, SCHEMA_SESSION, Session,
};
use slop_ninja_admission_wire::{
    ErrorCode, RequestScope, UPLOAD_MEDIA_TYPE, empty_sha256, header, path, request_message,
    response_limit,
};
use slop_ninja_protocol::submission::SubmissionUpload;
use slop_ninja_protocol::{AccountId, ChainBinding, Digest, Record, object_sha256};
use url::Url;

/// Signs request messages with the miner's hotkey.
pub trait RequestSigner: Send + Sync {
    fn hotkey(&self) -> AccountId;
    fn sign<'a>(&'a self, message: &'a [u8; 32]) -> BoxFuture<'a, Result<[u8; 64]>>;
}

/// An rtao signer (an unlocked wallet hotkey). Signs the exact 32 bytes with
/// no `<Bytes>` wrapper.
pub struct RtaoSigner(pub Arc<dyn rtao_signer::Signer>);

impl RequestSigner for RtaoSigner {
    fn hotkey(&self) -> AccountId {
        AccountId(self.0.account().0)
    }
    fn sign<'a>(&'a self, message: &'a [u8; 32]) -> BoxFuture<'a, Result<[u8; 64]>> {
        Box::pin(async move {
            self.0
                .sign_raw(message)
                .await
                .map_err(|error| anyhow::anyhow!("hotkey signing failed: {error}"))
        })
    }
}

/// SS58 (prefix 42) of a public key.
pub fn ss58(account: &AccountId) -> String {
    rtao_primitives::AccountId(account.0).to_string()
}

pub fn parse_ss58(text: &str) -> Result<AccountId> {
    let account: rtao_primitives::AccountId = text
        .parse()
        .map_err(|_| anyhow::anyhow!("{text:?} is not an SS58 address"))?;
    Ok(AccountId(account.0))
}

/// Requires HTTPS, or HTTP to a loopback host; no credentials, query or
/// fragment. Returns the URL with a trailing slash so paths join under it.
pub fn base_url(text: &str) -> Result<Url> {
    let mut url = Url::parse(text).with_context(|| format!("{text:?} is not a URL"))?;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name == "localhost",
        None => false,
    };
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "{text:?} must use https (plain http only to a loopback host)"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "{text:?} must not carry credentials, a query or a fragment"
    );
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// The agent every request names: the coordinator and status hosts, and the
/// chain RPC handshake ([`crate::chain::client_config`]).
pub const USER_AGENT: &str = concat!("slop_ninja_miner/", env!("CARGO_PKG_VERSION"));

/// The HTTP client: rustls with ring and the system's trust roots, no
/// redirects. `timeout` bounds every request without an object body; a
/// presigned PUT gets its own bound from its size (see [`Transfer`]).
pub fn http_client(timeout: Duration) -> Result<reqwest::Client> {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    let (added, _) = roots.add_parsable_certificates(native.certs);
    ensure!(added > 0, "no usable system TLS trust roots");
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(30)))
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .build()?)
}

/// How long a request may take.
#[derive(Clone, Copy, Debug)]
pub struct Transfer {
    /// Bound on a request without an object body.
    pub timeout: Duration,
    /// Slowest sustained upload rate this client waits for on a presigned
    /// PUT, in bytes per second: a local bound on a stalled connection
    /// (`timeout + len / min_upload_rate`), never a protocol condition. R2
    /// checks a URL's expiry when the PUT starts, so a transfer in flight
    /// completes at any rate.
    pub min_upload_rate: u64,
}

impl Transfer {
    pub fn put_timeout(&self, len: u64) -> Duration {
        let secs = len.div_ceil(self.min_upload_rate.max(1));
        self.timeout.saturating_add(Duration::from_secs(secs))
    }
}

impl Default for Transfer {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            min_upload_rate: 32 * 1024,
        }
    }
}

/// An admission API error.
#[derive(Debug)]
pub enum ApiError {
    /// The coordinator answered with a contract error.
    Api {
        status: u16,
        body: Box<ErrorBody>,
        /// `Retry-After`, in seconds, when the coordinator sent one.
        retry_after: Option<Duration>,
    },
    /// The coordinator answered something outside the contract.
    Protocol(String),
    /// The request did not complete.
    Transport(reqwest::Error),
}

impl ApiError {
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Api { body, .. } => ErrorCode::parse(&body.error.code),
            _ => None,
        }
    }
    pub fn body(&self) -> Option<&ErrorBody> {
        match self {
            Self::Api { body, .. } => Some(body),
            _ => None,
        }
    }
    /// Whether retrying the same request can succeed.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Api { body, .. } => body.error.retryable,
            Self::Transport(_) => true,
            Self::Protocol(_) => false,
        }
    }
    /// The wait the coordinator asked for.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Api { status, body, .. } => {
                write!(f, "{} ({status}): {}", body.error.code, body.error.message)?;
                let detail = &body.error.detail;
                if let Some(session) = &detail.session {
                    write!(f, " [session {session}]")?;
                }
                if let Some(missing) = &detail.missing {
                    write!(f, " [{} objects missing]", missing.len())?;
                }
                if let Some(cohort) = detail.admitting {
                    write!(f, " [admitting cohort {cohort}]")?;
                }
                if let Some(cohort) = detail.payload_cohort {
                    write!(f, " [the chain's commitment names cohort {cohort}]")?;
                }
                Ok(())
            }
            Self::Protocol(text) => write!(f, "unexpected response: {text}"),
            Self::Transport(error) => write!(f, "request failed: {error}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// Reads `GET /admission/v2/cohort` (unsigned, never from a cache).
pub async fn cohort_info(http: &reqwest::Client, base: &Url) -> Result<CohortInfo, ApiError> {
    let url = join(base, path::COHORT)?;
    let response = http
        .get(url)
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .map_err(ApiError::Transport)?;
    let info: CohortInfo = decode(response, response_limit()).await?;
    if info.schema != SCHEMA_COHORT {
        return Err(ApiError::Protocol(format!(
            "cohort schema {:?}",
            info.schema
        )));
    }
    Ok(info)
}

fn join(base: &Url, route: &str) -> Result<Url, ApiError> {
    base.join(route.trim_start_matches('/'))
        .map_err(|error| ApiError::Protocol(format!("bad route {route}: {error}")))
}

/// Reads at most `limit` bytes of a response body.
pub async fn read_bounded(
    response: &mut reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ApiError> {
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(ApiError::Protocol(format!(
            "a response body over the {limit}-byte limit"
        )));
    }
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ApiError::Transport)? {
        if out.len() + chunk.len() > limit {
            return Err(ApiError::Protocol(format!(
                "a response body over the {limit}-byte limit"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(reqwest::header::RETRY_AFTER)?;
    let secs: u64 = value.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(secs))
}

async fn decode<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<T, ApiError> {
    let status = response.status();
    if status.is_redirection() {
        return Err(ApiError::Protocol(format!(
            "{status}: the coordinator redirected the request; redirects are never followed"
        )));
    }
    let wait = retry_after(&response);
    let bytes = read_bounded(&mut response, limit).await?;
    if status.is_success() {
        return serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::Protocol(format!("{status}: {error}")));
    }
    match serde_json::from_slice::<ErrorBody>(&bytes) {
        Ok(body) => Err(ApiError::Api {
            status: status.as_u16(),
            body: Box::new(body),
            retry_after: wait,
        }),
        // An edge or platform error page (a gateway failure, an
        // overloaded or restarted Worker) carries no contract body: the
        // request may not have reached the coordinator, so it is retried as
        // `unavailable`.
        Err(_) if edge_unavailable(status.as_u16()) => Err(ApiError::Api {
            status: status.as_u16(),
            body: Box::new(ErrorBody::new(
                ErrorCode::Unavailable,
                format!("{status} from the edge without a contract error body"),
                Default::default(),
            )),
            retry_after: wait,
        }),
        Err(_) => Err(ApiError::Protocol(format!(
            "{status} without a contract error body"
        ))),
    }
}

/// Statuses an edge or platform error page uses for a request that did not
/// complete there: 502, 503 and 504 (a Worker over its limits answers 503),
/// and Cloudflare's 52x origin errors. A 5xx with a contract body is the
/// coordinator's own answer and keeps its code.
pub fn edge_unavailable(status: u16) -> bool {
    matches!(status, 502..=504 | 520..=527 | 530)
}

/// A hotkey's signed session with the coordinator.
pub struct AdmissionClient {
    http: reqwest::Client,
    base: Url,
    chain: ChainBinding,
    signer: Arc<dyn RequestSigner>,
    /// The coordinator's latest finalized block seen in a response.
    cursor: u64,
    validity: u64,
    transfer: Transfer,
    /// How presigned URLs are checked and where their PUTs go.
    put_policy: crate::direct::PutPolicy,
    /// The block of this wallet's confirmed commitment for the opening being
    /// uploaded, sent with session creation (`x-sn-commitment-block`) so a
    /// coordinator that has not logged it yet waits for it rather than
    /// answering with an older commitment.
    commitment_block: Option<u64>,
}

impl AdmissionClient {
    /// Reads the cohort parameters and binds the client to the coordinator's
    /// chain, which must be `expected` when given.
    pub async fn connect(
        http: reqwest::Client,
        base: Url,
        signer: Arc<dyn RequestSigner>,
        expected: Option<&ChainBinding>,
    ) -> Result<(Self, CohortInfo)> {
        let info = cohort_info(&http, &base).await?;
        let chain = info
            .chain
            .binding()
            .context("the coordinator reported a malformed chain binding")?;
        if let Some(expected) = expected {
            ensure!(
                *expected == chain,
                "the coordinator serves another chain or subnet than the opening names"
            );
        }
        let client = Self {
            http,
            base,
            chain,
            signer,
            cursor: info.finalized_block,
            validity: info.request_validity_blocks,
            transfer: Transfer::default(),
            put_policy: crate::direct::PutPolicy::default(),
            commitment_block: None,
        };
        Ok((client, info))
    }

    pub fn with_transfer(mut self, transfer: Transfer) -> Self {
        self.transfer = transfer;
        self
    }

    /// Sets how presigned URLs are checked (the trust file's pinned upload
    /// host) and where their PUTs go.
    pub fn with_put_policy(mut self, policy: crate::direct::PutPolicy) -> Self {
        self.put_policy = policy;
        self
    }

    pub fn transfer(&self) -> Transfer {
        self.transfer
    }

    pub fn put_policy(&self) -> &crate::direct::PutPolicy {
        &self.put_policy
    }

    /// The HTTP client (no redirects), shared by the presigned PUTs.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Sets the confirmed commitment block that session creation sends
    /// (`None`: send none).
    pub fn set_commitment_block(&mut self, block: Option<u64>) {
        self.commitment_block = block;
    }

    pub fn chain(&self) -> &ChainBinding {
        &self.chain
    }

    pub fn hotkey(&self) -> AccountId {
        self.signer.hotkey()
    }

    /// The coordinator's origin, as `scheme://host[:port]`.
    pub fn origin(&self) -> String {
        self.base.origin().ascii_serialization()
    }

    /// Re-reads the cohort parameters and the coordinator's cursor.
    pub async fn refresh(&mut self) -> Result<CohortInfo, ApiError> {
        let info = cohort_info(&self.http, &self.base).await?;
        self.observe(info.finalized_block);
        self.validity = info.request_validity_blocks;
        Ok(info)
    }

    fn observe(&mut self, finalized: u64) {
        self.cursor = self.cursor.max(finalized);
    }

    /// Half the validity window past the latest cursor, so the coordinator's
    /// cursor can advance between requests without expiring the request.
    fn valid_until(&self) -> u64 {
        self.cursor.saturating_add(self.validity / 2)
    }

    fn limit(&self) -> usize {
        response_limit()
    }

    async fn send(
        &self,
        method: Method,
        route: &str,
        body: Option<(Bytes, &'static str)>,
        content_sha256: Digest,
        hints: &[(&'static str, String)],
    ) -> Result<reqwest::Response, ApiError> {
        let hotkey = self.signer.hotkey();
        let valid_until = self.valid_until();
        let message = request_message(&RequestScope {
            chain: &self.chain,
            hotkey: &hotkey,
            valid_until,
            method: method.as_str(),
            path: route,
            content_sha256: &content_sha256,
        });
        let signature = self
            .signer
            .sign(&message)
            .await
            .map_err(|error| ApiError::Protocol(error.to_string()))?;
        let mut request = self
            .http
            .request(method, join(&self.base, route)?)
            .header(header::HOTKEY, ss58(&hotkey))
            .header(header::VALID_UNTIL, valid_until.to_string())
            .header(header::CONTENT_SHA256, content_sha256.hex())
            .header(header::SIGNATURE, hex::encode(signature));
        for (name, value) in hints {
            request = request.header(*name, value);
        }
        if let Some((bytes, media_type)) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, media_type)
                .body(bytes);
        }
        request.send().await.map_err(ApiError::Transport)
    }

    /// Sends a signed request, re-signing once when it expired. The re-signed
    /// request starts from the cursor `request_expired` carries, or from a
    /// fresh cohort read when it carries none.
    async fn signed<T: serde::de::DeserializeOwned>(
        &mut self,
        method: Method,
        route: &str,
        body: Option<(Bytes, &'static str)>,
        content_sha256: Digest,
    ) -> Result<(StatusCode, T), ApiError> {
        self.signed_with(method, route, body, content_sha256, &[])
            .await
    }

    /// [`Self::signed`], with unsigned hint headers (the confirmed
    /// commitment block, a declared upload rate).
    async fn signed_with<T: serde::de::DeserializeOwned>(
        &mut self,
        method: Method,
        route: &str,
        body: Option<(Bytes, &'static str)>,
        content_sha256: Digest,
        hints: &[(&'static str, String)],
    ) -> Result<(StatusCode, T), ApiError> {
        let mut renewed = false;
        loop {
            let response = self
                .send(method.clone(), route, body.clone(), content_sha256, hints)
                .await?;
            let status = response.status();
            match decode::<T>(response, self.limit()).await {
                Err(error) if error.code() == Some(ErrorCode::RequestExpired) && !renewed => {
                    renewed = true;
                    let cursor = error.body().and_then(|b| b.error.detail.finalized_block);
                    match cursor {
                        Some(cursor) if cursor > self.cursor => self.observe(cursor),
                        _ => {
                            self.refresh().await?;
                        }
                    }
                }
                Err(error) => return Err(error),
                Ok(value) => return Ok((status, value)),
            }
        }
    }

    fn checked(&mut self, session: Session) -> Result<Session, ApiError> {
        if session.schema != SCHEMA_SESSION {
            return Err(ApiError::Protocol(format!(
                "session schema {:?}",
                session.schema
            )));
        }
        self.observe(session.finalized_block);
        Ok(session)
    }

    /// Opens or returns the session for `upload`. `true` when it was created.
    /// An existing session is returned in whatever state it is; while its
    /// header is not recorded, the answer carries a fresh header URL.
    pub async fn create_session(
        &mut self,
        upload: &SubmissionUpload,
    ) -> Result<(Session, bool), ApiError> {
        let bytes = upload
            .to_bytes()
            .map_err(|error| ApiError::Protocol(format!("envelope: {error:?}")))?;
        // The signed body digest of one fixed-size envelope record.
        let digest = object_sha256(&bytes);
        let hints: Vec<(&'static str, String)> = self
            .commitment_block
            .map(|block| (header::COMMITMENT_BLOCK, block.to_string()))
            .into_iter()
            .collect();
        let (status, session) = self
            .signed_with::<Session>(
                Method::POST,
                path::SESSIONS,
                Some((Bytes::from(bytes), UPLOAD_MEDIA_TYPE)),
                digest,
                &hints,
            )
            .await?;
        let session = self.checked(session)?;
        let expected = upload
            .digest()
            .map_err(|error| ApiError::Protocol(format!("envelope: {error:?}")))?;
        if session.session != expected.hex() {
            return Err(ApiError::Protocol(
                "the session id is not the envelope digest".into(),
            ));
        }
        Ok((session, status == StatusCode::CREATED))
    }

    pub async fn session(&mut self, session: &str) -> Result<Session, ApiError> {
        let (_, value) = self
            .signed::<Session>(Method::GET, &path::session(session), None, empty_sha256())
            .await?;
        self.checked(value)
    }

    /// Fresh presigned URLs for every object the coordinator has not
    /// recorded present (settling the header first).
    pub async fn urls(&mut self, session: &str) -> Result<Session, ApiError> {
        let (_, value) = self
            .signed::<Session>(Method::POST, &path::urls(session), None, empty_sha256())
            .await?;
        self.checked(value)
    }

    /// Asks the coordinator to record every object R2 holds as committed
    /// and, when all are present, to queue the admission run.
    pub async fn complete(&mut self, session: &str) -> Result<Session, ApiError> {
        let (_, value) = self
            .signed::<Session>(Method::POST, &path::complete(session), None, empty_sha256())
            .await?;
        self.checked(value)
    }

    /// Fresh URLs for recorded objects that went missing (`repair_needed`).
    pub async fn repair(&mut self, session: &str) -> Result<Session, ApiError> {
        let (_, value) = self
            .signed::<Session>(Method::POST, &path::repair(session), None, empty_sha256())
            .await?;
        self.checked(value)
    }

    pub async fn hotkey_view(&mut self, cohort: u64) -> Result<HotkeyView, ApiError> {
        let hotkey = ss58(&self.signer.hotkey());
        let (_, view) = self
            .signed::<HotkeyView>(
                Method::GET,
                &path::hotkey(cohort, &hotkey),
                None,
                empty_sha256(),
            )
            .await?;
        if view.schema != SCHEMA_HOTKEY {
            return Err(ApiError::Protocol(format!(
                "hotkey schema {:?}",
                view.schema
            )));
        }
        if view.hotkey != hotkey || view.cohort != cohort {
            return Err(ApiError::Protocol(
                "the coordinator answered for another hotkey or cohort".into(),
            ));
        }
        self.observe(view.finalized_block);
        Ok(view)
    }
}

/// Fails unless `session` belongs to this envelope.
pub fn check_session_matches(session: &Session, upload: &SubmissionUpload) -> Result<()> {
    if session.cohort != upload.opening.cohort
        || session.commitment != upload.commitment.hex()
        || session.header_sha256 != upload.header_sha256.hex()
        || session.header_len != upload.header_len
        || session.hotkey != ss58(&upload.opening.hotkey)
    {
        bail!("the coordinator's session does not describe this upload");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_timeouts_grow_with_the_object() {
        let transfer = Transfer {
            timeout: Duration::from_secs(300),
            min_upload_rate: 32 * 1024,
        };
        assert_eq!(transfer.put_timeout(0), Duration::from_secs(300));
        // A 64 MiB file at the floor rate gets 2,048 s on top of the base.
        assert_eq!(
            transfer.put_timeout(64 << 20),
            Duration::from_secs(300 + 2_048)
        );
        let zero = Transfer {
            min_upload_rate: 0,
            ..transfer
        };
        assert_eq!(zero.put_timeout(10), Duration::from_secs(310));
    }
}

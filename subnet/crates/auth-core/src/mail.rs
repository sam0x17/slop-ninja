//! Sign-in mail: the kinds and their copy, and the sending seam.
//!
//! Every kind has its own subject and body, so a connect mail never looks
//! like a sign-in mail; each has a text part and a minimal HTML part, with no
//! tracking pixel and no link but the one to the configured site origin. The
//! token travels in the URL fragment (`/auth/email/confirm#t=<token>`), which
//! browsers never send, so it reaches no log, `Referer` or link-rewriting
//! proxy, and opening the link signs nobody in by itself.
//!
//! The recipient is an [`EmailNorm`], which only [`crate::email_addr`]
//! builds: mail goes to the identity's subject and to nothing else.
//!
//! [`Mailer`] is the sending seam. On wasm32 the account Worker uses
//! `cloudflare::CloudflareMailer` (Cloudflare Email Sending through the
//! `send_email` binding); local development uses `Capture` (feature `dev`),
//! which keeps each message for the tests and the auth stub instead.

use slop_ninja_account_wire::{Bucket, MailIntent, NoRequester, UserId, mail_buckets};
use slop_ninja_detect_page::Origin;

use crate::email_addr::EmailNorm;
use crate::providers::OAuthProvider;
use crate::token::Token;

/// The display name every sign-in mail is sent under.
pub const FROM_NAME: &str = "SlopNinja";
/// The confirm page's path; the token follows in the fragment.
pub const CONFIRM_PATH: &str = "/auth/email/confirm";

/// A mail kind. Each is a different message. Its intent, stored with the
/// token, is the account contract's [`MailIntent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MailKind {
    /// "Your SlopNinja sign-in link".
    Signin,
    /// "Connect this address to a SlopNinja account" (opened in the same
    /// browser, while signed in).
    Link,
    /// "Confirm it's you on SlopNinja".
    StepUp,
    /// "Connect your ‹provider› sign-in to SlopNinja".
    AttachPending {
        /// The provider whose sign-in found this address in use.
        provider: OAuthProvider,
    },
    /// Account recovery after the operator's accepted `attach_email` op
    /// (refused at launch; its copy awaits approval before that op opens).
    Recover,
}

impl MailKind {
    /// The intent stored with the token (`email_tokens.intent`).
    #[must_use]
    pub const fn intent(self) -> MailIntent {
        match self {
            Self::Signin => MailIntent::Signin,
            Self::Link => MailIntent::Link,
            Self::StepUp => MailIntent::StepUp,
            Self::AttachPending { .. } => MailIntent::AttachPending,
            Self::Recover => MailIntent::Recover,
        }
    }

    /// The kind of a stored intent: `provider` is the pending row's provider
    /// and is needed (and used) only for `attach_pending`.
    #[must_use]
    pub const fn of(intent: MailIntent, provider: Option<OAuthProvider>) -> Option<Self> {
        match (intent, provider) {
            (MailIntent::Signin, _) => Some(Self::Signin),
            (MailIntent::Link, _) => Some(Self::Link),
            (MailIntent::StepUp, _) => Some(Self::StepUp),
            (MailIntent::AttachPending, Some(provider)) => Some(Self::AttachPending { provider }),
            (MailIntent::AttachPending, None) => None,
            (MailIntent::Recover, _) => Some(Self::Recover),
        }
    }

    /// The quota buckets a send of this kind to the address `addr_h` draws
    /// on ([`mail_buckets`]): only mail whose link the holder can use draws
    /// on the per-address sign-in bucket; connect mail draws on its own two
    /// and needs the requesting account, `requester`.
    pub fn buckets(
        self,
        addr_h: &[u8; 32],
        requester: Option<UserId>,
    ) -> Result<Vec<Bucket>, NoRequester> {
        mail_buckets(self.intent(), &hex::encode(addr_h), requester)
    }
}

/// Sender and link settings, all from config.
#[derive(Clone, Debug)]
pub struct MailConfig {
    /// The sender address, from config (the account Worker's `MAIL_FROM`
    /// var): an address on the onboarded sending domain that the
    /// `send_email` binding allows.
    pub from_address: String,
    /// The origin links point to (`SITE_ORIGIN`).
    pub link_origin: Origin,
    /// `MAIL_SUBJECT_PREFIX` (for example `"[staging] "`), empty in
    /// production.
    pub subject_prefix: String,
    /// Whether to add `Auto-Submitted: auto-generated`. Off until that
    /// header is confirmed on the sending service's custom-header allow-list.
    pub auto_submitted: bool,
}

/// Why a mail config was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// The sender is not a valid address in normal form.
    FromAddress,
    /// The subject prefix is not printable ASCII of at most
    /// [`slop_ninja_account_wire::MAX_SUBJECT_PREFIX`] bytes
    /// ([`slop_ninja_account_wire::mail_subject_prefix`]).
    SubjectPrefix,
}

impl MailConfig {
    /// Builds and checks a config.
    pub fn new(
        from_address: &str,
        link_origin: Origin,
        subject_prefix: &str,
        auto_submitted: bool,
    ) -> Result<Self, ConfigError> {
        if EmailNorm::from_stored(from_address).is_none() {
            return Err(ConfigError::FromAddress);
        }
        let subject_prefix = slop_ninja_account_wire::mail_subject_prefix(subject_prefix)
            .map_err(|_| ConfigError::SubjectPrefix)?;
        Ok(Self {
            from_address: from_address.to_owned(),
            link_origin,
            subject_prefix: subject_prefix.to_owned(),
            auto_submitted,
        })
    }
}

/// One message, ready for a [`Mailer`].
#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    /// The sender's display name.
    pub from_name: &'static str,
    /// The sender address.
    pub from_address: String,
    /// The recipient: always the identity's normalized subject.
    pub to: EmailNorm,
    /// The subject.
    pub subject: String,
    /// The text part.
    pub text: String,
    /// The HTML part.
    pub html: String,
    /// Extra headers.
    pub headers: Vec<(&'static str, &'static str)>,
}

impl core::fmt::Debug for Message {
    // The bodies carry a sign-in credential: never print them.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Message")
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}

/// The paragraphs of one kind's body; the link goes between the first and
/// the rest.
fn copy(kind: MailKind, minutes: u64) -> (&'static str, String, String) {
    match kind {
        MailKind::Signin => (
            "Your SlopNinja sign-in link",
            "Sign in to SlopNinja:".to_owned(),
            format!(
                "The link works once, for {minutes} minutes. If you didn't ask for it, ignore this email."
            ),
        ),
        MailKind::Link => (
            "Connect this address to a SlopNinja account",
            "Someone signed in to SlopNinja asked to connect this address to their account. If that was you, open this link in the same browser, while signed in:".to_owned(),
            format!(
                "It works once, for {minutes} minutes. If you didn't ask for this, ignore this email. Nothing changes and nobody is signed in."
            ),
        ),
        MailKind::StepUp => (
            "Confirm it's you on SlopNinja",
            "Open this link in the browser where you're signed in to confirm it's you:".to_owned(),
            format!(
                "It works once, for {minutes} minutes. If you didn't ask for this, ignore this email."
            ),
        ),
        MailKind::AttachPending { provider } => (
            match provider {
                OAuthProvider::Google => "Connect your Google sign-in to SlopNinja",
                OAuthProvider::GitHub => "Connect your GitHub sign-in to SlopNinja",
                OAuthProvider::X => "Connect your X sign-in to SlopNinja",
            },
            format!(
                "Someone tried to sign in to SlopNinja with {} using this address. If that was you, open this link in the same browser to sign in and connect it:",
                provider.display_name()
            ),
            format!(
                "It works once, for {minutes} minutes. If you didn't ask for this, ignore this email."
            ),
        ),
        MailKind::Recover => (
            "Sign in to your SlopNinja account",
            "Your request to recover your SlopNinja account was accepted. Open this link to sign in:".to_owned(),
            format!(
                "It works once, for {minutes} minutes. If you didn't ask for this, ignore this email."
            ),
        ),
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The confirm link for `token`.
#[must_use]
pub fn confirm_link(origin: &Origin, token: &Token) -> String {
    format!("{}{CONFIRM_PATH}#t={}", origin.as_str(), token.encode())
}

/// Composes one message. `minutes` is the link's lifetime as the reader is
/// told it (the email link lifetime, or less when an attach-pending row
/// expires sooner).
#[must_use]
pub fn compose(
    config: &MailConfig,
    kind: MailKind,
    to: &EmailNorm,
    token: &Token,
    minutes: u64,
) -> Message {
    let (subject, lead, tail) = copy(kind, minutes);
    let link = confirm_link(&config.link_origin, token);
    let text = format!("{lead}\n\n{link}\n\n{tail}\n");
    let link_html = escape_html(&link);
    let html = format!(
        "<!doctype html><html><body><p>{}</p><p><a href=\"{link_html}\">{link_html}</a></p><p>{}</p></body></html>",
        escape_html(&lead),
        escape_html(&tail)
    );
    let mut headers = Vec::new();
    if config.auto_submitted {
        headers.push(("Auto-Submitted", "auto-generated"));
    }
    Message {
        from_name: FROM_NAME,
        from_address: config.from_address.clone(),
        to: to.clone(),
        subject: format!("{}{subject}", config.subject_prefix),
        text,
        html,
        headers,
    }
}

/// A send's failure, mapped from the sending service's error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendFailure {
    /// `E_RECIPIENT_SUPPRESSED`: add the address to the suppression list;
    /// counts as sent.
    Suppressed,
    /// `E_RATE_LIMIT_EXCEEDED`: same answer to the reader; raises the
    /// `auth_mail_quota` alert point.
    RateLimited,
    /// `E_DAILY_LIMIT_EXCEEDED`: as [`SendFailure::RateLimited`].
    DailyLimit,
    /// Any other code (or none): a content-free metrics point.
    Other(OtherCode),
}

/// The other codes, kept as a closed set for the metrics point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtherCode {
    /// `E_SENDER_NOT_VERIFIED`.
    SenderNotVerified,
    /// `E_SENDER_DOMAIN_NOT_AVAILABLE`.
    SenderDomainNotAvailable,
    /// `E_RECIPIENT_NOT_ALLOWED`.
    RecipientNotAllowed,
    /// `E_DELIVERY_FAILED`.
    DeliveryFailed,
    /// Anything else.
    Unknown,
}

impl SendFailure {
    /// Maps an error's `code` property.
    #[must_use]
    pub fn from_code(code: Option<&str>) -> Self {
        match code.unwrap_or("") {
            "E_RECIPIENT_SUPPRESSED" => Self::Suppressed,
            "E_RATE_LIMIT_EXCEEDED" => Self::RateLimited,
            "E_DAILY_LIMIT_EXCEEDED" => Self::DailyLimit,
            "E_SENDER_NOT_VERIFIED" => Self::Other(OtherCode::SenderNotVerified),
            "E_SENDER_DOMAIN_NOT_AVAILABLE" => Self::Other(OtherCode::SenderDomainNotAvailable),
            "E_RECIPIENT_NOT_ALLOWED" => Self::Other(OtherCode::RecipientNotAllowed),
            "E_DELIVERY_FAILED" => Self::Other(OtherCode::DeliveryFailed),
            _ => Self::Other(OtherCode::Unknown),
        }
    }

    /// Whether the send counts as sent (the reader's answer is the same in
    /// every case; this decides the bookkeeping).
    #[must_use]
    pub const fn counts_as_sent(self) -> bool {
        matches!(self, Self::Suppressed)
    }

    /// Whether it raises the `auth_mail_quota` alert point.
    #[must_use]
    pub const fn is_quota(self) -> bool {
        matches!(self, Self::RateLimited | Self::DailyLimit)
    }

    /// The alert point it raises, if any
    /// ([`slop_ninja_detect_page::ae::account::AUTH_MAIL_QUOTA`]).
    #[must_use]
    pub const fn alert(self) -> Option<&'static str> {
        if self.is_quota() {
            Some(slop_ninja_detect_page::ae::account::AUTH_MAIL_QUOTA)
        } else {
            None
        }
    }

    /// The `result_code` of the account dataset's `mail` point
    /// ([`slop_ninja_detect_page::ae::account::MAIL`]): a closed set, so the
    /// point never carries free text from the sending service.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Suppressed => "suppressed",
            Self::RateLimited => "rate_limited",
            Self::DailyLimit => "daily_limit",
            Self::Other(OtherCode::SenderNotVerified) => "sender_not_verified",
            Self::Other(OtherCode::SenderDomainNotAvailable) => "sender_domain_not_available",
            Self::Other(OtherCode::RecipientNotAllowed) => "recipient_not_allowed",
            Self::Other(OtherCode::DeliveryFailed) => "delivery_failed",
            Self::Other(OtherCode::Unknown) => "unknown",
        }
    }
}

/// The sending seam.
#[allow(async_fn_in_trait)] // Workers futures are single-threaded.
pub trait Mailer {
    /// Sends one message.
    async fn send(&self, message: &Message) -> Result<(), SendFailure>;
}

/// Cloudflare Email Sending through a `send_email` binding.
#[cfg(target_arch = "wasm32")]
pub mod cloudflare {
    use wasm_bindgen::{JsCast, JsValue};

    use super::{Mailer, Message, SendFailure};

    /// The account Worker's mailer: `SendEmail::send_with_builder`.
    pub struct CloudflareMailer {
        binding: worker::SendEmail,
    }

    impl CloudflareMailer {
        /// Wraps the binding (`env.send_email("SIGNIN_EMAIL")`).
        #[must_use]
        pub fn new(binding: worker::SendEmail) -> Self {
            Self { binding }
        }
    }

    impl Mailer for CloudflareMailer {
        async fn send(&self, m: &Message) -> Result<(), SendFailure> {
            let from = worker::EmailAddress::new(m.from_name, &m.from_address);
            let builder = worker::SendEmailBuilder::new_with_email_address_and_str(
                &from,
                m.to.as_str(),
                &m.subject,
            );
            builder.set_text(&m.text);
            builder.set_html(&m.html);
            if !m.headers.is_empty() {
                let headers = js_sys::Object::new();
                for (k, v) in &m.headers {
                    js_sys::Reflect::set(&headers, &JsValue::from_str(k), &JsValue::from_str(v))
                        .map_err(|_| SendFailure::Other(super::OtherCode::Unknown))?;
                }
                builder.set_headers(headers.unchecked_ref());
            }
            match self.binding.send_with_builder(&builder).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    let code = js_sys::Reflect::get(&e, &JsValue::from_str("code"))
                        .ok()
                        .and_then(|v| v.as_string());
                    Err(SendFailure::from_code(code.as_deref()))
                }
            }
        }
    }
}

/// Local development: a mailer that keeps every message.
#[cfg(feature = "dev")]
pub mod capture {
    use std::cell::RefCell;

    use serde::Serialize;

    use super::{Mailer, Message, SendFailure};

    /// One captured message, in the record shape the auth stub's `/mail`
    /// route accepts.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    pub struct Captured {
        /// The recipient, byte for byte as the mailer received it.
        pub to: String,
        /// The sender address.
        pub from: String,
        /// The subject.
        pub subject: String,
        /// The text part.
        pub text: String,
        /// The HTML part.
        pub html: String,
    }

    impl Captured {
        /// The JSON record.
        #[must_use]
        pub fn to_json(&self) -> String {
            serde_json::to_string(self).expect("a record always serializes")
        }
    }

    impl From<&Message> for Captured {
        fn from(m: &Message) -> Self {
            Self {
                to: m.to.as_str().to_owned(),
                from: m.from_address.clone(),
                subject: m.subject.clone(),
                text: m.text.clone(),
                html: m.html.clone(),
            }
        }
    }

    /// Keeps messages in memory; can be told to fail the next send.
    #[derive(Default)]
    pub struct Capture {
        sent: RefCell<Vec<Captured>>,
        fail_next: RefCell<Option<SendFailure>>,
    }

    impl Capture {
        /// An empty capture.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        /// Makes the next send fail with `failure` (and capture nothing).
        pub fn fail_next(&self, failure: SendFailure) {
            *self.fail_next.borrow_mut() = Some(failure);
        }

        /// Every message captured so far.
        #[must_use]
        pub fn sent(&self) -> Vec<Captured> {
            self.sent.borrow().clone()
        }

        /// Takes every message captured so far.
        pub fn take(&self) -> Vec<Captured> {
            core::mem::take(&mut *self.sent.borrow_mut())
        }
    }

    impl Mailer for Capture {
        async fn send(&self, message: &Message) -> Result<(), SendFailure> {
            if let Some(f) = self.fail_next.borrow_mut().take() {
                return Err(f);
            }
            self.sent.borrow_mut().push(Captured::from(message));
            Ok(())
        }
    }

    /// In local `wrangler dev`, where a Worker has no file system: posts
    /// each message's record to the auth stub's `/mail` route over a
    /// dev-only service binding, for the tests to read back.
    #[cfg(target_arch = "wasm32")]
    pub struct ServiceCapture {
        stub: worker::Fetcher,
    }

    #[cfg(target_arch = "wasm32")]
    impl ServiceCapture {
        /// Wraps the stub's service binding.
        #[must_use]
        pub fn new(stub: worker::Fetcher) -> Self {
            Self { stub }
        }
    }

    #[cfg(target_arch = "wasm32")]
    impl Mailer for ServiceCapture {
        async fn send(&self, message: &Message) -> Result<(), SendFailure> {
            let body = Captured::from(message).to_json();
            let headers = worker::Headers::new();
            headers
                .set("Content-Type", "application/json")
                .map_err(|_| SendFailure::Other(super::OtherCode::Unknown))?;
            let mut init = worker::RequestInit::new();
            init.with_method(worker::Method::Post)
                .with_headers(headers)
                .with_body(Some(body.into()));
            let req = worker::Request::new_with_init("https://auth-stub/mail", &init)
                .map_err(|_| SendFailure::Other(super::OtherCode::Unknown))?;
            match self.stub.fetch_request(req).await {
                Ok(r) if (200..300).contains(&r.status_code()) => Ok(()),
                _ => Err(SendFailure::Other(super::OtherCode::DeliveryFailed)),
            }
        }
    }
}

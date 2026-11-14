//! The URL pipeline's two outside services, as traits the flows run on:
//!
//! - [`FetchApi`]: the fetch Worker behind the `DETECT_FETCH` service
//!   binding (`/fetch`, `/x/post`, `/x/check`; `detect-fetch-wire`);
//! - [`HostApi`]: the `DetectHost` Durable Object of a registrable domain:
//!   the security screen, robots.txt, the politeness slot and the
//!   reputation of the domain.
//!
//! The Worker implements them over its bindings (wasm32 only); the native
//! tests implement them with stubs that count every call.

use slop_ninja_detect_fetch_wire::{FetchAnswer, FetchRequest, XCheckAnswer, XCheckRequest, XPostAnswer, XPostRequest};
use slop_ninja_detect_page::codes::Reputation;

use crate::db::IoError;

/// The fetch Worker.
#[allow(async_fn_in_trait)]
pub trait FetchApi {
    /// One page fetch and extraction under a politeness slot.
    async fn fetch(&self, req: &FetchRequest) -> Result<FetchAnswer, IoError>;
    /// One post on X through X's free embed.
    async fn x_post(&self, req: &XPostRequest) -> Result<XPostAnswer, IoError>;
    /// The existence check of a batch of posts, controls first.
    async fn x_check(&self, req: &XCheckRequest) -> Result<XCheckAnswer, IoError>;
}

/// The security screen's verdict on a host (1.1.1.1 for Families'
/// security resolver: `0.0.0.0` means listed as malware or phishing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Clear,
    SecurityRisk,
}

/// What `DetectHost` answers for a fetch of one URL of its domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The fetch may run now under this slot (one at a time per domain).
    Granted { slot: String },
    /// robots.txt disallows the path for SlopNinjaBot, or asks for no AI
    /// input (`robots`).
    Disallowed,
    /// robots.txt's content signal says `search=no` (`private_source`).
    NoSearch,
    /// Another fetch holds the domain, or its `Crawl-delay` or `Retry-After`
    /// has not passed: try again at this time (ms UTC).
    WaitUntil(u64),
}

/// The `DetectHost` object of one registrable domain.
#[allow(async_fn_in_trait)]
pub trait HostApi {
    /// The security screen of `host` (cached for its DNS TTL).
    async fn screen(&self, registrable: &str, host: &str) -> Result<Screen, IoError>;
    /// A politeness slot for `url` (its robots.txt read first when the
    /// cached rules are stale), held at most `lease_ms`.
    async fn slot(&self, registrable: &str, url: &str, lease_ms: u64) -> Result<Slot, IoError>;
    /// The fetch under `slot` ended; `retry_after_ms` is a `Retry-After` the
    /// site sent.
    async fn release(&self, registrable: &str, slot: &str, retry_after_ms: Option<u64>);
    /// The domain's reputation and the ranking list's version: `ranked` when
    /// the domain is in the ranking shards and not blocked by the family
    /// screen, `adult` when the family screen blocks it, else `unranked`.
    async fn reputation(&self, registrable: &str) -> Result<(Reputation, String), IoError>;
    /// Waits until `t` (ms UTC) or returns at once when `t` has passed.
    async fn wait_until(&self, t: u64);
}

/// The IndexNow endpoint (one POST of a ping body).
#[allow(async_fn_in_trait)]
pub trait Pinger {
    async fn indexnow(&self, body: Vec<u8>) -> Result<(), IoError>;
}

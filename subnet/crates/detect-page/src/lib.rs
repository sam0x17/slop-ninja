//! Contracts of the SlopNinja detection pages.
//!
//! One crate fixes what the Workers on a page's path exchange, so the detect
//! Worker (producer), the website and secret-host Workers (renderers), the
//! account Worker, the card renderer and the test harness agree on one set of
//! types and strings:
//!
//! - [`page`]: the page contract `slop_ninja_detect_page_v2` ([`Page`]);
//! - [`refusal`]: every refusal code the Workers emit, its status and copy key;
//! - [`slug`]: slug and token drawing, the public blocklist, folding and the
//!   page paths with their 301 forms;
//! - [`ninja`]: the ninja's state rule and the gauge's ends ([`GaugeEnds`]);
//! - [`card`]: the card JSON and its typed link ([`CardLink`]);
//! - [`origins`]: the two public origins, from config ([`Origins`]);
//! - [`sn`]: the `X-SN-Host`, `X-SN-Route` and `X-SN-Principal` headers, the
//!   forwarded route ids and the internal routes;
//! - [`internal`] and [`api`]: request and answer bodies;
//! - [`queue`]: the `detect-jobs` queue's messages;
//! - [`ae`]: the Analytics Engine point schema;
//! - [`switches`]: the kill switches;
//! - [`x`]: posts on X: the canonical post URL and the page's link to the
//!   post ([`PostUrl`]);
//! - [`codes`]: the closed value sets, each with one wire string.
//!
//! The crate is pure (no I/O, no runtime) and compiles natively and for
//! wasm32. JSON fixtures of every contract sit in `tests/fixtures/` and are
//! checked byte for byte (`tests/golden.rs`).

pub mod ae;
pub mod api;
pub mod card;
pub mod codes;
pub mod fmt;
pub mod ids;
pub mod internal;
pub mod ninja;
pub mod origins;
pub mod page;
pub mod queue;
pub mod refusal;
pub mod slug;
pub mod sn;
pub mod switches;
pub mod x;

pub use card::{Card, CardLink};
pub use codes::*;
pub use ids::{IdH, UserId};
pub use ninja::{GaugeEnds, NinjaInput, ninja_state};
pub use origins::{Origin, Origins};
pub use page::{PAGE_SCHEMA, Page};
pub use refusal::{ApiRefusal, RefusalCode};
pub use sn::{Principal, RouteId};
pub use x::{PostUrl, XPost};

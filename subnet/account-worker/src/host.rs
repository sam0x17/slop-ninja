//! The platform seam: everything the handlers ask of the runtime.
//!
//! The handlers are plain async functions of a [`Host`]: the D1 store, the
//! `QuotaLimiter` Durable Object, the mail binding, outbound HTTP, the
//! `detect-jobs` queue, Analytics Engine, the CSPRNG and the clock. On
//! Cloudflare (`cf.rs`) each is its binding; the native tests implement the
//! same trait over the account store's own migration in SQLite, an in-memory
//! quota map, a capturing mailer and the auth stub's provider routes, so the
//! SQL the Worker runs is the SQL the tests run.
//!
//! **Statements.** A [`Stmt`] is SQL with `?` placeholders and its values.
//! [`Host::batch`] runs its statements in order as one transaction (D1's
//! `batch`): when any statement fails, nothing in the batch is applied. A
//! guard ([`guard`]) is a statement that fails, and so rolls the batch back,
//! when its condition does not hold.

use slop_ninja_auth_core::mail::{Message, SendFailure};
use slop_ninja_auth_core::policy::TokenBucket;
use slop_ninja_detect_page::ae::Point;
use slop_ninja_detect_page::queue::QueueMessage;

/// One SQL value. Integers stay below 2^53, so they survive D1's JS numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Null,
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for Val {
    fn from(v: i64) -> Val {
        Val::Int(v)
    }
}

impl From<u64> for Val {
    fn from(v: u64) -> Val {
        Val::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}

impl From<u32> for Val {
    fn from(v: u32) -> Val {
        Val::Int(i64::from(v))
    }
}

impl From<bool> for Val {
    fn from(v: bool) -> Val {
        Val::Int(i64::from(v))
    }
}

impl From<&str> for Val {
    fn from(v: &str) -> Val {
        Val::Text(v.to_owned())
    }
}

impl From<String> for Val {
    fn from(v: String) -> Val {
        Val::Text(v)
    }
}

impl From<&[u8]> for Val {
    fn from(v: &[u8]) -> Val {
        Val::Blob(v.to_vec())
    }
}

impl From<Vec<u8>> for Val {
    fn from(v: Vec<u8>) -> Val {
        Val::Blob(v)
    }
}

impl From<[u8; 32]> for Val {
    fn from(v: [u8; 32]) -> Val {
        Val::Blob(v.to_vec())
    }
}

impl From<[u8; 16]> for Val {
    fn from(v: [u8; 16]) -> Val {
        Val::Blob(v.to_vec())
    }
}

impl<T: Into<Val>> From<Option<T>> for Val {
    fn from(v: Option<T>) -> Val {
        v.map_or(Val::Null, Into::into)
    }
}

/// One statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub sql: String,
    pub args: Vec<Val>,
}

impl Stmt {
    pub fn new(sql: impl Into<String>, args: Vec<Val>) -> Stmt {
        Stmt { sql: sql.into(), args }
    }
}

/// `Stmt` from SQL and a list of values: `stmt!("…", a, b)`.
#[macro_export]
macro_rules! stmt {
    ($sql:expr $(, $arg:expr)* $(,)?) => {
        $crate::host::Stmt::new($sql, vec![$($crate::host::Val::from($arg)),*])
    };
}

/// A guard: a statement that inserts nothing when `condition` (an SQL
/// expression over `args`) holds, and otherwise violates `schema_info.v NOT
/// NULL`, which fails the statement and rolls back its whole batch.
pub fn guard(condition: &str, args: Vec<Val>) -> Stmt {
    Stmt::new(format!("INSERT INTO schema_info (k, v) SELECT '{GUARD_KEY}', NULL WHERE NOT ({condition})"), args)
}

/// The key a failing guard tries to insert.
pub const GUARD_KEY: &str = "guard";

/// One result row: column names and values, in the statement's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub cols: Vec<(String, Val)>,
}

impl Row {
    pub fn get(&self, col: &str) -> Option<&Val> {
        self.cols.iter().find(|(c, _)| c == col).map(|(_, v)| v)
    }

    pub fn int(&self, col: &str) -> Option<i64> {
        match self.get(col)? {
            Val::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn u64(&self, col: &str) -> Option<u64> {
        self.int(col).and_then(|i| u64::try_from(i).ok())
    }

    pub fn text(&self, col: &str) -> Option<&str> {
        match self.get(col)? {
            Val::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn blob(&self, col: &str) -> Option<&[u8]> {
        match self.get(col)? {
            Val::Blob(b) => Some(b),
            _ => None,
        }
    }

    pub fn b32(&self, col: &str) -> Option<[u8; 32]> {
        self.blob(col)?.try_into().ok()
    }

    pub fn b16(&self, col: &str) -> Option<[u8; 16]> {
        self.blob(col)?.try_into().ok()
    }
}

/// Why a statement or batch failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbError {
    /// A [`guard`] failed: the batch's precondition did not hold, and
    /// nothing was applied.
    Guard,
    /// A UNIQUE or other constraint failed: nothing was applied.
    Constraint,
    /// The store could not be reached or the statement failed otherwise.
    Unavailable,
}

/// Classifies a store error message (SQLite's text, as D1 and rusqlite both
/// pass it on).
pub fn classify(message: &str) -> DbError {
    if message.contains("NOT NULL constraint failed: schema_info.v") {
        DbError::Guard
    } else if message.contains("constraint failed") || message.contains("SQLITE_CONSTRAINT") {
        DbError::Constraint
    } else {
        DbError::Unavailable
    }
}

/// An outbound HTTP request (a provider call or the X avatar fetch).
#[derive(Clone, PartialEq, Eq)]
pub struct OutRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    /// The call's whole budget, ms: the platform aborts it, the body read
    /// included.
    pub timeout_ms: u64,
    /// The longest body read; a longer body answers `body: None`.
    pub max_body: usize,
}

impl core::fmt::Debug for OutRequest {
    // Headers and bodies carry secrets, codes and bearer tokens.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OutRequest").field("method", &self.method).field("url", &self.url).finish_non_exhaustive()
    }
}

/// An outbound answer. Redirects are never followed: a 3xx is answered as
/// it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutReply {
    pub status: u16,
    /// `None` when the body was longer than the request's `max_body`.
    pub body: Option<Vec<u8>>,
}

/// Why an outbound call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    /// The budget ran out.
    Timeout,
    /// The connection failed.
    Failed,
}

/// The answer of one take from a `QuotaLimiter` bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaTake {
    pub taken: bool,
    /// This take left the bucket empty (raises the ceiling alert).
    pub emptied: bool,
}

/// The quota object could not be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaUnavailable;

/// A queue send failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueUnavailable;

/// The runtime.
#[allow(async_fn_in_trait)] // Workers futures are single-threaded.
pub trait Host {
    /// Every row of one statement.
    async fn all(&self, stmt: Stmt) -> Result<Vec<Row>, DbError>;
    /// One transaction: each statement's rows, or nothing applied.
    async fn batch(&self, stmts: Vec<Stmt>) -> Result<Vec<Vec<Row>>, DbError>;
    /// Takes one unit from the bucket named `name` (the hex HMAC name),
    /// sized `size`.
    async fn quota_take(&self, name: &str, size: TokenBucket) -> Result<QuotaTake, QuotaUnavailable>;
    /// Refills the bucket named `name`.
    async fn quota_reset(&self, name: &str) -> Result<(), QuotaUnavailable>;
    /// Sends one sign-in mail.
    async fn send_mail(&self, message: &Message) -> Result<(), SendFailure>;
    /// One outbound request.
    async fn http(&self, req: &OutRequest) -> Result<OutReply, NetError>;
    /// Sends one `detect-jobs` message.
    async fn enqueue(&self, msg: &QueueMessage) -> Result<(), QueueUnavailable>;
    /// Writes one content-free Analytics Engine point: `point`'s name, then
    /// `blobs` in its order.
    fn point(&self, point: &Point, blobs: &[&str]);
    /// Fills `buf` from the CSPRNG.
    fn fill(&self, buf: &mut [u8]);
    /// The wall clock, Unix milliseconds.
    fn now_ms(&self) -> u64;

    /// The first row, if any.
    async fn first(&self, stmt: Stmt) -> Result<Option<Row>, DbError> {
        Ok(self.all(stmt).await?.into_iter().next())
    }

    /// `N` random bytes.
    fn random<const N: usize>(&self) -> [u8; N] {
        let mut b = [0u8; N];
        self.fill(&mut b);
        b
    }
}

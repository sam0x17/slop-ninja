//! The store's I/O seam: SQL statements, rows and the [`Io`] trait the
//! flows run on.
//!
//! Every statement the store runs is written once, as a constant beside
//! the flow that runs it, and executed by one of two backends: D1 through
//! its Sessions API in the Worker (`cf_store`), and an in-memory SQLite in
//! the native tests, on the same migration with foreign keys on. A flow is an `async fn` over [`Io`],
//! so the tests drive the same code the Worker runs.
//!
//! - Values are integers, text, blobs and NULL ([`Val`]). Blob columns are
//!   always read through `hex(col) AS col` (and parsed back with
//!   [`Row::hex32`]), so neither backend's blob decoding matters; blobs are
//!   bound as parameters only.
//! - A [`Row`] is the statement's columns by name, so a statement's reader
//!   names what it reads.
//! - [`Sql::batch`] runs statements in one transaction: all or none, as a D1
//!   batch does.
//!
//! No error carries a statement's values or text ([`DbError`] is a code).

use std::borrow::Cow;

use slop_ninja_account_wire::OwnedAddRequest;
use slop_ninja_detect_page::queue::QueueMessage;

/// One SQL value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Null,
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

impl Val {
    pub fn int(v: u64) -> Val {
        Val::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }

    pub fn opt_int(v: Option<u64>) -> Val {
        v.map_or(Val::Null, Val::int)
    }

    pub fn text(v: impl Into<String>) -> Val {
        Val::Text(v.into())
    }

    pub fn opt_text(v: Option<impl Into<String>>) -> Val {
        v.map_or(Val::Null, |s| Val::Text(s.into()))
    }

    pub fn blob(v: &[u8]) -> Val {
        Val::Blob(v.to_vec())
    }

    pub fn opt_blob(v: Option<&[u8]>) -> Val {
        v.map_or(Val::Null, Val::blob)
    }
}

/// One statement and its positional parameters (`?1`, `?2`, ... or `?`).
#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub sql: Cow<'static, str>,
    pub params: Vec<Val>,
}

impl Stmt {
    pub fn new(sql: &'static str, params: Vec<Val>) -> Stmt {
        Stmt { sql: Cow::Borrowed(sql), params }
    }

    pub fn owned(sql: String, params: Vec<Val>) -> Stmt {
        Stmt { sql: Cow::Owned(sql), params }
    }
}

/// One result row: its columns by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row(pub Vec<(String, Val)>);

impl Row {
    pub fn get(&self, name: &str) -> Option<&Val> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    /// An integer column; `None` for NULL or another type.
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.get(name)? {
            Val::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn u64(&self, name: &str) -> Option<u64> {
        self.int(name).and_then(|v| u64::try_from(v).ok())
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        match self.get(name)? {
            Val::Text(s) => Some(s),
            _ => None,
        }
    }

    /// A blob column read as `hex(col) AS col`: 64 hex digits of either case.
    pub fn hex32(&self, name: &str) -> Option<[u8; 32]> {
        let s = self.text(name)?;
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).ok()?;
        Some(out)
    }
}

/// One statement's result: its rows and the rows it changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Done {
    pub rows: Vec<Row>,
    pub changes: u64,
}

impl Done {
    pub fn first(&self) -> Option<&Row> {
        self.rows.first()
    }
}

/// A store error: a fixed code, never a statement's text or values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbError {
    /// The database refused or failed the statement.
    Failed,
    /// A row did not have the shape its reader expects.
    Shape,
}

/// Another service's error (R2, the queue, a binding): content-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoError;

impl From<DbError> for IoError {
    fn from(_: DbError) -> IoError {
        IoError
    }
}

/// SQL on the detect store.
#[allow(async_fn_in_trait)]
pub trait Sql {
    /// One statement.
    async fn run(&self, s: Stmt) -> Result<Done, DbError>;
    /// Statements in one transaction: all or none.
    async fn batch(&self, v: Vec<Stmt>) -> Result<Vec<Done>, DbError>;
}

/// One page of an R2 listing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocPage {
    pub objects: Vec<(String, u64)>,
    pub cursor: Option<String>,
}

/// The most keys one R2 delete takes.
pub const R2_DELETE_KEYS: usize = 1000;

/// Everything a store flow touches besides SQL.
#[allow(async_fn_in_trait)]
pub trait Io: Sql {
    /// Milliseconds UTC.
    fn now_ms(&self) -> u64;
    /// A deploy-time var, trimmed; `None` when unset or empty.
    fn var(&self, name: &str) -> Option<String>;
    /// Bytes from the platform's CSPRNG.
    fn fill(&self, buf: &mut [u8]);
    /// R2 `detect-docs`: put one object.
    async fn doc_put(&self, key: &str, bytes: Vec<u8>) -> Result<(), IoError>;
    /// R2: one object, `None` when missing.
    async fn doc_get(&self, key: &str) -> Result<Option<Vec<u8>>, IoError>;
    /// R2: delete at most [`R2_DELETE_KEYS`] keys; a missing key is not an
    /// error, so a delete can be repeated.
    async fn doc_delete(&self, keys: &[String]) -> Result<(), IoError>;
    /// R2: one page of objects under `prefix` (key and upload time in ms),
    /// and the cursor of the next page.
    async fn doc_list(&self, prefix: &str, cursor: Option<&str>) -> Result<DocPage, IoError>;
    /// The account Worker's `owned/add`.
    async fn owned_add(&self, req: &OwnedAddRequest) -> Result<(), IoError>;
    /// The `detect-jobs` queue: send these messages (one `sendBatch`).
    async fn enqueue(&self, msgs: Vec<QueueMessage>) -> Result<(), IoError>;
    /// The zone cache purge of these URLs; `Ok(false)` when no purge token
    /// is configured (the cache bound then covers the delay).
    async fn cache_purge(&self, urls: &[String]) -> Result<bool, IoError>;
    /// The alerts relay's legal stream (`POST /v1/legal`).
    async fn relay_legal(&self, body: Vec<u8>) -> Result<(), IoError>;
    /// A feedback receipt for a shown result (from the feedback object);
    /// `None` when it cannot be issued.
    async fn receipt(&self, result: &crate::doc::StoredResult) -> Option<String>;
    /// One content-free Analytics Engine point: the point's name and its
    /// blobs in schema order.
    fn metric(&self, point: &'static str, blobs: &[&str]);
    /// One point with its double: a count (the X sweep's `xcheck` points)
    /// or an age in seconds (`xsweep`). A backend that keeps no doubles
    /// writes the point alone.
    fn metric_n(&self, point: &'static str, blobs: &[&str], double: f64) {
        let _ = double;
        self.metric(point, blobs);
    }
}

/// Splits `keys` into R2 delete calls and runs them in order; stops at the
/// first failure.
pub async fn delete_docs(io: &impl Io, keys: &[String]) -> Result<(), IoError> {
    for chunk in keys.chunks(R2_DELETE_KEYS) {
        io.doc_delete(chunk).await?;
    }
    Ok(())
}

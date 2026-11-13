//! The store's [`Io`] on Cloudflare (wasm32 only): D1 through its Sessions
//! API, R2 `detect-docs`, the `detect-jobs` queue, the account Worker's and
//! the alerts relay's service bindings, the zone cache purge, the feedback
//! object's receipts and Analytics Engine.
//!
//! - D1: one session per request (`first-unconstrained`; read replication
//!   is off at launch, so every read is the primary's). Values bind as
//!   numbers, strings, `null` and `Uint8Array` (blobs); rows come back as
//!   objects, read by column name, with blob columns selected as `hex()`.
//! - Every outbound call carries a deadline (one queue hold), and no call
//!   carries an `X-SN-*` header: the callees' internal routes refuse them.
//! - Nothing here writes a console line, and no error carries a statement's
//!   text, a value or a body.

use serde::Serialize;
use slop_ninja_account_wire::OwnedAddRequest;
use slop_ninja_detect_page::queue::QueueMessage;
use wasm_bindgen::JsValue;
use worker::{Bucket, D1DatabaseSession, D1PreparedStatement, Env, Method};

use crate::cf_io;
use crate::db::{DbError, DocPage, Done, Io, IoError, Row, Sql, Stmt, Val};
use crate::doc::StoredResult;

/// The detect store (D1).
pub(crate) const STORE: &str = "DETECT_STORE";
/// The documents bucket (R2).
pub(crate) const DOCS: &str = "DETECT_DOCS";
/// The jobs queue (producer).
pub(crate) const JOBS: &str = "DETECT_JOBS";
/// The account Worker (service binding).
pub(crate) const ACCOUNT: &str = "ACCOUNT";
/// The alerts relay (service binding).
pub(crate) const RELAY: &str = "ALERTS_RELAY";
/// The zone cache purge token (optional secret) and zone id (var).
pub(crate) const PURGE_TOKEN: &str = "CACHE_PURGE_TOKEN";
pub(crate) const PURGE_ZONE: &str = "CACHE_PURGE_ZONE";
/// The largest answer read from a binding.
const ANSWER_BYTES: usize = 64 * 1024;
/// The largest document read from R2: a secret page's document is bounded
/// by the share body, a paid document by its job's bound; both are far
/// under this.
pub(crate) const DOC_BYTES: usize = 16 * 1024 * 1024;

/// The store's I/O for one invocation.
pub(crate) struct CfStore<'e> {
    env: &'e Env,
    db: D1DatabaseSession,
    docs: Bucket,
    /// Every outbound call's deadline.
    deadline_ms: u64,
}

impl<'e> CfStore<'e> {
    /// `None` when a binding is missing (the route answers `not_configured`).
    pub(crate) fn new(env: &'e Env, deadline_ms: u64) -> Option<CfStore<'e>> {
        let db = env.d1(STORE).ok()?.with_session(Some("first-unconstrained")).ok()?;
        let docs = env.bucket(DOCS).ok()?;
        Some(CfStore { env, db, docs, deadline_ms })
    }

    fn prepare(&self, s: &Stmt) -> Result<D1PreparedStatement, DbError> {
        let params: Vec<JsValue> = s
            .params
            .iter()
            .map(|v| match v {
                Val::Null => JsValue::NULL,
                // Every integer the store binds is a time, a count or a row
                // id, far under 2^53.
                Val::Int(i) => JsValue::from_f64(*i as f64),
                Val::Text(t) => JsValue::from_str(t),
                Val::Blob(b) => js_sys::Uint8Array::from(b.as_slice()).into(),
            })
            .collect();
        self.db.prepare(s.sql.as_ref()).bind(&params).map_err(|_| DbError::Failed)
    }

    async fn post_json<B: Serialize>(&self, binding: &str, url: &str, body: &B) -> Result<(), IoError> {
        let fetcher = self.env.service(binding).map_err(|_| IoError)?;
        let text = serde_json::to_string(body).map_err(|_| IoError)?;
        let mut req =
            cf_io::internal_request(url, Method::Post, Some(&text), Some(self.deadline_ms)).map_err(|_| IoError)?;
        let _ = req.headers_mut().map(|h| h.set("Content-Type", "application/json"));
        let mut res = fetcher.fetch_request(req).await.map_err(|_| IoError)?;
        let ok = (200..300).contains(&res.status_code());
        cf_io::discard(&mut res);
        if ok { Ok(()) } else { Err(IoError) }
    }
}

fn val_of(v: &serde_json::Value) -> Result<Val, DbError> {
    use serde_json::Value as J;
    Ok(match v {
        J::Null => Val::Null,
        J::Bool(b) => Val::Int(i64::from(*b)),
        J::String(s) => Val::Text(s.clone()),
        J::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Val::Int(i),
            (None, Some(f)) if f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 => Val::Int(f as i64),
            _ => return Err(DbError::Shape),
        },
        // A blob D1 returned as an array of bytes (the store reads blobs
        // through `hex()`, so this only covers a statement that does not).
        J::Array(a) => Val::Blob(
            a.iter()
                .map(|x| x.as_u64().and_then(|b| u8::try_from(b).ok()).ok_or(DbError::Shape))
                .collect::<Result<_, _>>()?,
        ),
        J::Object(_) => return Err(DbError::Shape),
    })
}

fn done_of(r: &worker::D1Result) -> Result<Done, DbError> {
    let objs = r.results::<serde_json::Map<String, serde_json::Value>>().map_err(|_| DbError::Shape)?;
    let mut rows = Vec::with_capacity(objs.len());
    for o in &objs {
        let mut row = Vec::with_capacity(o.len());
        for (k, v) in o {
            row.push((k.clone(), val_of(v)?));
        }
        rows.push(Row(row));
    }
    let changes = r.meta().ok().flatten().and_then(|m| m.changes).unwrap_or(0) as u64;
    Ok(Done { rows, changes })
}

impl Sql for CfStore<'_> {
    async fn run(&self, s: Stmt) -> Result<Done, DbError> {
        let st = self.prepare(&s)?;
        let r = st.all().await.map_err(|_| DbError::Failed)?;
        done_of(&r)
    }

    async fn batch(&self, v: Vec<Stmt>) -> Result<Vec<Done>, DbError> {
        let stmts = v.iter().map(|s| self.prepare(s)).collect::<Result<Vec<_>, _>>()?;
        let rs = self.db.batch(stmts).await.map_err(|_| DbError::Failed)?;
        rs.iter().map(done_of).collect()
    }
}

#[derive(Serialize)]
struct IssueReq {
    band: &'static str,
    length: &'static str,
}

#[derive(serde::Deserialize)]
struct IssueReply {
    #[serde(default)]
    receipt: Option<String>,
}

#[derive(Serialize)]
struct PurgeBody<'a> {
    files: &'a [String],
}

impl Io for CfStore<'_> {
    fn now_ms(&self) -> u64 {
        cf_io::now_ms()
    }

    fn var(&self, name: &str) -> Option<String> {
        self.env.var(name).ok().map(|v| v.to_string().trim().to_owned()).filter(|v| !v.is_empty())
    }

    fn fill(&self, buf: &mut [u8]) {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let b = crate::cf::random_bytes(len);
        b.copy_to(buf);
    }

    async fn doc_put(&self, key: &str, bytes: Vec<u8>) -> Result<(), IoError> {
        if cf_io::r2_put(&self.docs, key, &bytes, Some(self.deadline_ms)).await { Ok(()) } else { Err(IoError) }
    }

    async fn doc_get(&self, key: &str) -> Result<Option<Vec<u8>>, IoError> {
        cf_io::r2_bytes(&self.docs, key, DOC_BYTES, Some(self.deadline_ms)).await.map_err(|_| IoError)
    }

    async fn doc_delete(&self, keys: &[String]) -> Result<(), IoError> {
        if keys.is_empty() {
            return Ok(());
        }
        self.docs.delete_multiple(keys.iter().map(String::as_str).collect()).await.map_err(|_| IoError)
    }

    async fn doc_list(&self, prefix: &str, cursor: Option<&str>) -> Result<DocPage, IoError> {
        let mut b = self.docs.list().prefix(prefix).limit(1000);
        if let Some(c) = cursor {
            b = b.cursor(c);
        }
        let objs = b.execute().await.map_err(|_| IoError)?;
        let objects = objs.objects().iter().map(|o| (o.key(), o.uploaded().as_millis())).collect();
        let cursor = if objs.truncated() { objs.cursor() } else { None };
        Ok(DocPage { objects, cursor })
    }

    async fn owned_add(&self, req: &OwnedAddRequest) -> Result<(), IoError> {
        self.post_json(ACCOUNT, "https://account/internal/v1/owned/add", req).await
    }

    async fn enqueue(&self, msgs: Vec<QueueMessage>) -> Result<(), IoError> {
        if msgs.is_empty() {
            return Ok(());
        }
        let q = self.env.queue(JOBS).map_err(|_| IoError)?;
        q.send_batch(msgs).await.map_err(|_| IoError)
    }

    async fn cache_purge(&self, urls: &[String]) -> Result<bool, IoError> {
        let (Ok(token), Some(zone)) = (self.env.secret(PURGE_TOKEN), self.var(PURGE_ZONE)) else { return Ok(false) };
        let url = format!("https://api.cloudflare.com/client/v4/zones/{zone}/purge_cache");
        let text = serde_json::to_string(&PurgeBody { files: urls }).map_err(|_| IoError)?;
        let mut req =
            cf_io::internal_request(&url, Method::Post, Some(&text), Some(self.deadline_ms)).map_err(|_| IoError)?;
        if let Ok(h) = req.headers_mut() {
            let _ = h.set("Content-Type", "application/json");
            let _ = h.set("Authorization", &format!("Bearer {token}"));
        }
        let mut res = worker::Fetch::Request(req).send().await.map_err(|_| IoError)?;
        let ok = (200..300).contains(&res.status_code());
        cf_io::discard(&mut res);
        if ok { Ok(true) } else { Err(IoError) }
    }

    async fn relay_legal(&self, body: Vec<u8>) -> Result<(), IoError> {
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| IoError)?;
        self.post_json(RELAY, "https://relay/v1/legal", &value).await
    }

    async fn receipt(&self, r: &StoredResult) -> Option<String> {
        use slop_ninja_detect_page::codes::{Band, ShortResult};
        let band = match (r.band, r.short) {
            (Some(Band::VeryStrong), None) => "very_strong",
            (Some(Band::Strong), None) | (None, Some(ShortResult::Strong)) => "strong",
            (Some(Band::Unclear), None) => "unclear",
            (Some(Band::Weak), None) => "weak",
            (None, Some(ShortResult::None)) => "no_strong_signs",
            _ => return None,
        };
        let length = r.length?.as_str();
        let ns = self.env.durable_object("DETECT_FEEDBACK").ok()?;
        let stub = ns.id_from_name("feedback").ok()?.get_stub().ok()?;
        let text = serde_json::to_string(&IssueReq { band, length }).ok()?;
        let req = cf_io::internal_request("https://feedback/issue", Method::Post, Some(&text), Some(self.deadline_ms))
            .ok()?;
        let mut res = stub.fetch_with_request(req).await.ok()?;
        if res.status_code() != 200 {
            cf_io::discard(&mut res);
            return None;
        }
        cf_io::response_json::<IssueReply>(&mut res, ANSWER_BYTES).await.ok()?.receipt
    }

    fn metric(&self, point: &'static str, blobs: &[&str]) {
        let Ok(ds) = self.env.analytics_engine(crate::cf::METRICS) else { return };
        let mut all: Vec<&str> = Vec::with_capacity(blobs.len() + 1);
        all.push(point);
        all.extend_from_slice(blobs);
        let _ = worker::AnalyticsEngineDataPointBuilder::new().indexes([point]).blobs(all).write_to(&ds);
    }
}

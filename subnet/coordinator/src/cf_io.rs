//! Bounded body reads and platform-cancelled timeouts for the Workers entry
//! points (wasm32 only).
//!
//! workers-rs 0.8.7 reads a body whole (`Request::bytes` copies the JS
//! ArrayBuffer; `Response::bytes` appends chunks with no bound; an R2
//! `ObjectBody::bytes` first holds the whole object as one JS ArrayBuffer),
//! and its `ByteStream` (wasm-streams) leaks a forgotten closure into the
//! scope when dropped before the end. These readers use the stream's own
//! reader instead, so JS holds one chunk at a time. They refuse a declared
//! length above the caller's bound before anything is read, and admit the
//! buffer as a block of the current scope only once bytes arrive: the
//! declared length when it is exact, else a buffer grown with the bytes
//! actually received (doubling, each growth one admitted block, never past
//! the bound). A client that opens a request and sends nothing holds no
//! block. A stream longer than its bound or, when exact, than its declared
//! length is refused.
//!
//! A declared length is exact only for an unencoded body. workerd decodes a
//! `gzip` or `br` fetch reply on the fly but keeps the encoded
//! `Content-Length`, so for an encoded body the declared length says nothing
//! about the decoded bytes: the read grows with the decoded bytes and stops
//! at the caller's bound. RPC calls also ask for `Accept-Encoding: identity`.
//!
//! Timeouts never drop a pending future (design section 2.6, rule 1): a
//! Durable Object call carries an `AbortSignal.timeout` the platform
//! enforces on the call and its reply (`internal_request`), and an await the
//! platform cannot cancel (an R2 operation, a body stream read, a Workers AI
//! run) is raced against a timer in JS (`settle`), so the promise Rust
//! awaits always settles. The operation's late outcome is then discarded by
//! JS, never by dropping a Rust future whose callbacks still live in the
//! scope.

use js_sys::{Function, Reflect, Uint8Array};
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use worker::{Request, Response};

#[wasm_bindgen(inline_js = "
export function cfio_within(p, ms) {
  let t;
  const timer = new Promise((r) => { t = setTimeout(() => r(null), ms); });
  return Promise.race([Promise.resolve(p).then((v) => ({ v }), (e) => ({ e })), timer]).finally(() => clearTimeout(t));
}
")]
extern "C" {
    /// `p` raced against a timer of `ms`: resolves to `{v}` (fulfilled),
    /// `{e}` (rejected) or `null` (the timer fired first). It never rejects.
    fn cfio_within(p: &js_sys::Promise, ms: f64) -> js_sys::Promise;
}

/// The wall clock (`Date.now()`).
pub fn now_ms() -> u64 {
    js_sys::Date::now() as u64
}

/// How an await raced against a deadline ended.
pub enum Settled {
    Value(JsValue),
    /// The promise rejected, with its reason.
    Error(JsValue),
    Timeout,
}

/// Awaits `promise` until `deadline_ms` (absolute, `now_ms` clock; `None`:
/// no deadline). Past the deadline it answers `Timeout`; the promise's own
/// outcome then arrives in JS and is discarded there.
pub async fn settle(promise: js_sys::Promise, deadline_ms: Option<u64>) -> Settled {
    let Some(until) = deadline_ms else {
        return match JsFuture::from(promise).await {
            Ok(v) => Settled::Value(v),
            Err(e) => Settled::Error(e),
        };
    };
    let left = until.saturating_sub(now_ms());
    if left == 0 {
        return Settled::Timeout;
    }
    match JsFuture::from(cfio_within(&promise, left as f64)).await {
        Ok(r) if r.is_null() || r.is_undefined() => Settled::Timeout,
        Ok(r) if Reflect::has(&r, &JsValue::from_str("v")).unwrap_or(false) => match Reflect::get(&r, &JsValue::from_str("v")) {
            Ok(v) => Settled::Value(v),
            Err(e) => Settled::Error(e),
        },
        Ok(r) => Settled::Error(Reflect::get(&r, &JsValue::from_str("e")).unwrap_or(JsValue::UNDEFINED)),
        Err(e) => Settled::Error(e),
    }
}

/// An internal Durable Object request to `url`: an optional string body and
/// an `AbortSignal.timeout(timeout_ms)` the platform enforces on the call and
/// its reply body (`None`: no deadline, for operator routes whose work has
/// no bound yet). workerd honours a Durable Object stub's request signal
/// (local workerd, 2026-11-02: a stub fetch to an object that answers after
/// 3 s rejects with `TimeoutError` at 0.5 s, in the Request and in the init
/// form).
pub fn internal_request(url: &str, method: worker::Method, body: Option<&str>, timeout_ms: Option<u64>) -> worker::Result<Request> {
    use worker::worker_sys::web_sys;
    let init = web_sys::RequestInit::new();
    init.set_method(method.as_ref());
    if let Some(body) = body {
        init.set_body(&JsValue::from_str(body));
    }
    if let Some(ms) = timeout_ms {
        let signal = web_sys::AbortSignal::timeout_with_u32(u32::try_from(ms).unwrap_or(u32::MAX));
        init.set_signal(Some(&signal));
    }
    Ok(Request::from(web_sys::Request::new_with_str_and_init(url, &init)?))
}

/// Why a bounded read failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyError {
    /// The declared or streamed length exceeds the bound.
    TooLarge,
    /// The current scope cannot hold the body (an arena refusal).
    Refused(slop_ninja_arena::RefusalKind),
    /// The stream failed, or ended short of its declared length.
    Transport,
    /// The body did not arrive whole before the read's deadline.
    Timeout,
}

impl BodyError {
    /// The HTTP status the routes answer with.
    pub fn status(self) -> u16 {
        match self {
            BodyError::TooLarge => 413,
            BodyError::Refused(slop_ninja_arena::RefusalKind::Invalid) => 413,
            BodyError::Timeout => 408,
            BodyError::Refused(_) | BodyError::Transport => 503,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            BodyError::TooLarge => "body_too_large",
            BodyError::Refused(_) => "arena_refusal",
            BodyError::Transport => "body_unreadable",
            BodyError::Timeout => "body_timeout",
        }
    }
}

/// Why a bounded JSON read failed: the body (its own status: an arena
/// refusal or a transport failure is a 503 the caller retries), or a body
/// that is not the expected type (400).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonError {
    Body(BodyError),
    Parse,
}

impl JsonError {
    pub fn status(self) -> u16 {
        match self {
            JsonError::Body(e) => e.status(),
            JsonError::Parse => 400,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            JsonError::Body(e) => e.code(),
            JsonError::Parse => "bad_internal_request",
        }
    }
}

fn declared(len: Option<String>) -> Option<usize> {
    len.and_then(|v| v.trim().parse::<usize>().ok())
}

/// Whether a `Content-Encoding` value names an encoding the platform decodes
/// (anything but absent, empty or `identity`): the declared length is then
/// the encoded length, not the body's.
fn encoded(value: Option<String>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        !v.is_empty() && !v.eq_ignore_ascii_case("identity")
    })
}

/// How a body's declared length bounds it.
#[derive(Clone, Copy)]
enum Length {
    /// The exact length (an unencoded body, an R2 object's size).
    Exact(usize),
    /// None declared, or one that does not describe the decoded bytes.
    Unknown,
}

impl Length {
    fn of(declared: Option<usize>, encoded: bool) -> Self {
        match declared {
            Some(d) if !encoded => Length::Exact(d),
            _ => Length::Unknown,
        }
    }
}

/// Admits one buffer of exactly `len` bytes as a block of the current scope.
fn admit(len: usize) -> Result<(), BodyError> {
    match slop_ninja_arena::admit_current_block(len, 1) {
        Ok(_) => Ok(()),
        Err(r) => Err(BodyError::Refused(r.kind)),
    }
}

/// The capacity a buffer of unknown final length grows to when `total`
/// bytes no longer fit `cap`: double, at least `total` and one page's block,
/// never past `bound`. Doubling keeps the blocks a growing read holds below
/// twice its body (a freed block is re-keyed by the scope's next one).
fn grown(cap: usize, total: usize, bound: usize) -> usize {
    let page = slop_ninja_arena::PAGE.saturating_sub(slop_ninja_arena::BLOCK_OVERHEAD);
    cap.saturating_mul(2).max(total).max(page).min(bound)
}

/// Calls `target[name]()`.
fn call0(target: &JsValue, name: &str) -> Result<JsValue, BodyError> {
    let f = Reflect::get(target, &JsValue::from_str(name)).map_err(|_| BodyError::Transport)?;
    let f: Function = f.dyn_into().map_err(|_| BodyError::Transport)?;
    f.call0(target).map_err(|_| BodyError::Transport)
}

/// Reads a JS `ReadableStream` through its own default reader, chunk by
/// chunk, each read awaited to completion or raced against `deadline_ms` in
/// JS (`settle`; no pending future is ever dropped, so nothing outlives the
/// scope). Nothing is admitted before the first chunk arrives: then the
/// exact length's block, or for an unknown length a buffer grown with the
/// bytes received. A refusal or a timeout cancels the stream through the
/// reader; the cancel promise is left to JS.
async fn read(stream: Option<JsValue>, length: Length, bound: usize, deadline_ms: Option<u64>) -> Result<Vec<u8>, BodyError> {
    let exact = match length {
        Length::Exact(d) if d > bound => return Err(BodyError::TooLarge),
        Length::Exact(d) => Some(d),
        Length::Unknown => None,
    };
    let limit = exact.unwrap_or(bound);
    let mut out: Vec<u8> = Vec::new();
    let Some(stream) = stream else {
        return if exact.unwrap_or(0) == 0 { Ok(out) } else { Err(BodyError::Transport) };
    };
    let reader = call0(&stream, "getReader")?;
    let refuse = |e: BodyError| {
        let _ = call0(&reader, "cancel");
        e
    };
    loop {
        let promise: js_sys::Promise = call0(&reader, "read")?.dyn_into().map_err(|_| BodyError::Transport)?;
        let step = match settle(promise, deadline_ms).await {
            Settled::Value(v) => v,
            Settled::Error(_) => return Err(BodyError::Transport),
            Settled::Timeout => return Err(refuse(BodyError::Timeout)),
        };
        if Reflect::get(&step, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            break;
        }
        let chunk: Uint8Array = Reflect::get(&step, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.dyn_into().ok())
            .ok_or(BodyError::Transport)
            .map_err(refuse)?;
        let n = chunk.length() as usize;
        let total = out.len().checked_add(n).ok_or(BodyError::TooLarge).map_err(refuse)?;
        if total > limit {
            return Err(refuse(BodyError::TooLarge));
        }
        if total > out.capacity() {
            // The first bytes of an exact body admit its whole block; an
            // unknown length grows with what has arrived.
            let cap = match exact {
                Some(d) => d,
                None => grown(out.capacity(), total, bound),
            };
            admit(cap).map_err(refuse)?;
            out.try_reserve_exact(cap - out.len()).map_err(|_| refuse(BodyError::Refused(slop_ninja_arena::RefusalKind::Retry)))?;
        }
        let start = out.len();
        out.resize(total, 0);
        if let Some(dst) = out.get_mut(start..total) {
            chunk.copy_to(dst);
        }
    }
    if exact.is_some_and(|d| out.len() != d) {
        return Err(BodyError::Transport);
    }
    Ok(out)
}

/// A request body of at most `bound` bytes. A declared length above the
/// bound is refused before the body is touched.
pub async fn request_bytes(req: &mut Request, bound: usize) -> Result<Vec<u8>, BodyError> {
    request_bytes_until(req, bound, None).await
}

/// [`request_bytes`] that must arrive whole before `deadline_ms` (absolute,
/// `now_ms` clock): past it the read is cancelled and answers `Timeout`.
pub async fn request_bytes_until(req: &mut Request, bound: usize, deadline_ms: Option<u64>) -> Result<Vec<u8>, BodyError> {
    let len = declared(req.headers().get("content-length").ok().flatten());
    let enc = encoded(req.headers().get("content-encoding").ok().flatten());
    let length = Length::of(len, enc);
    if let Length::Exact(d) = length
        && d > bound
    {
        return Err(BodyError::TooLarge);
    }
    let stream = req.inner().body().map(JsValue::from);
    read(stream, length, bound, deadline_ms).await
}

/// A request body of at most `bound` bytes, parsed as JSON (internal Durable
/// Object bodies).
pub async fn request_json<T: serde::de::DeserializeOwned>(req: &mut Request, bound: usize) -> Result<T, JsonError> {
    let raw = request_bytes(req, bound).await.map_err(JsonError::Body)?;
    serde_json::from_slice(&raw).map_err(|_| JsonError::Parse)
}

/// Cancels the body of a response the caller will not read (an error
/// answer, or a reply whose status is all it needs). workerd keeps a cron,
/// queue or `waitUntil` invocation open while a subrequest's body is unread
/// (verified in local workerd: a cron whose Durable Object call answered 503
/// with an unread body never completed; cancelling it completes at once).
pub fn discard(resp: &mut Response) {
    if let worker::ResponseBody::Stream(s) = resp.body() {
        let _ = call0(&JsValue::from(s.clone()), "cancel");
    }
}

/// A response body of at most `bound` bytes.
pub async fn response_bytes(resp: &mut Response, bound: usize) -> Result<Vec<u8>, BodyError> {
    let len = declared(resp.headers().get("content-length").ok().flatten());
    let enc = encoded(resp.headers().get("content-encoding").ok().flatten());
    let length = Length::of(len, enc);
    if let Length::Exact(d) = length
        && d > bound
    {
        return Err(BodyError::TooLarge);
    }
    match resp.body() {
        worker::ResponseBody::Empty => Ok(Vec::new()),
        worker::ResponseBody::Body(b) => {
            if b.len() > bound {
                return Err(BodyError::TooLarge);
            }
            admit(b.len())?;
            let mut out = Vec::new();
            out.try_reserve_exact(b.len()).map_err(|_| BodyError::Refused(slop_ninja_arena::RefusalKind::Retry))?;
            out.extend_from_slice(b);
            Ok(out)
        }
        worker::ResponseBody::Stream(s) => read(Some(JsValue::from(s.clone())), length, bound, None).await,
    }
}

/// A response body streamed into `sink` without being held: each JS chunk is
/// copied into one fixed scratch block (an arena page's block, admitted once
/// when the first bytes arrive) a window at a time, and `sink` sees every
/// window in order. Memory is that block whatever the body's size, so no
/// length bound applies; the call's own `AbortSignal` bounds it in time.
/// `sink` answering `false` stops the read (the stream is cancelled) and the
/// call answers `Ok`: the sink holds its own verdict. A stream that ends
/// short of an exact declared length is `Transport`, never a whole body.
pub async fn response_scan(resp: &mut Response, sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), BodyError> {
    let len = declared(resp.headers().get("content-length").ok().flatten());
    let enc = encoded(resp.headers().get("content-encoding").ok().flatten());
    let exact = match Length::of(len, enc) {
        Length::Exact(d) => Some(d),
        Length::Unknown => None,
    };
    let stream = match resp.body() {
        worker::ResponseBody::Empty => return if exact.unwrap_or(0) == 0 { Ok(()) } else { Err(BodyError::Transport) },
        worker::ResponseBody::Body(b) => {
            // Already in memory: no copy.
            sink(b);
            return Ok(());
        }
        worker::ResponseBody::Stream(s) => JsValue::from(s.clone()),
    };
    let reader = call0(&stream, "getReader")?;
    let refuse = |e: BodyError| {
        let _ = call0(&reader, "cancel");
        e
    };
    let window = slop_ninja_arena::PAGE.saturating_sub(slop_ninja_arena::BLOCK_OVERHEAD).max(1);
    let mut scratch: Vec<u8> = Vec::new();
    let mut total: usize = 0;
    loop {
        let promise: js_sys::Promise = call0(&reader, "read")?.dyn_into().map_err(|_| BodyError::Transport)?;
        let step = match settle(promise, None).await {
            Settled::Value(v) => v,
            Settled::Error(_) => return Err(BodyError::Transport),
            Settled::Timeout => return Err(refuse(BodyError::Timeout)),
        };
        if Reflect::get(&step, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            break;
        }
        let chunk: Uint8Array = Reflect::get(&step, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.dyn_into().ok())
            .ok_or(BodyError::Transport)
            .map_err(refuse)?;
        let n = chunk.length() as usize;
        total = total.checked_add(n).ok_or(BodyError::Transport).map_err(refuse)?;
        if exact.is_some_and(|d| total > d) {
            return Err(refuse(BodyError::Transport));
        }
        if n > 0 && scratch.capacity() == 0 {
            admit(window).map_err(refuse)?;
            scratch
                .try_reserve_exact(window)
                .map_err(|_| refuse(BodyError::Refused(slop_ninja_arena::RefusalKind::Retry)))?;
            scratch.resize(window, 0);
        }
        let mut from = 0;
        while from < n {
            let to = n.min(from.saturating_add(window));
            let Some(dst) = scratch.get_mut(..to - from) else {
                return Err(refuse(BodyError::Transport));
            };
            chunk.subarray(from as u32, to as u32).copy_to(dst);
            if !sink(dst) {
                let _ = call0(&reader, "cancel");
                return Ok(());
            }
            from = to;
        }
    }
    if exact.is_some_and(|d| total != d) {
        return Err(BodyError::Transport);
    }
    Ok(())
}

/// A Durable Object's JSON reply of at most `bound` bytes (its reply type's
/// bound), read like any response body; the call's own `AbortSignal` bounds
/// the read in time.
pub async fn response_json<T: serde::de::DeserializeOwned>(resp: &mut Response, bound: usize) -> Result<T, JsonError> {
    let raw = response_bytes(resp, bound).await.map_err(JsonError::Body)?;
    serde_json::from_slice(&raw).map_err(|_| JsonError::Parse)
}

/// An R2 object's bytes, read through the object body's own stream reader
/// (JS holds one chunk at a time; workers-rs's `ObjectBody::bytes` would hold
/// the whole object as one JS ArrayBuffer first). Its size, exact, is checked
/// against `bound` (the caller's per-object bound) before anything is read,
/// then admitted as one block of the current scope when its bytes arrive.
/// `deadline_ms` (absolute, `now_ms` clock; `None`: none) bounds the get and
/// every read in JS (`settle`). `Ok(None)`: no object.
pub async fn r2_bytes(bucket: &worker::Bucket, key: &str, bound: usize, deadline_ms: Option<u64>) -> Result<Option<Vec<u8>>, BodyError> {
    let b: &JsValue = bucket.as_ref();
    let get: Function = Reflect::get(b, &JsValue::from_str("get"))
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or(BodyError::Transport)?;
    let promise: js_sys::Promise =
        get.call1(b, &JsValue::from_str(key)).ok().and_then(|p| p.dyn_into().ok()).ok_or(BodyError::Transport)?;
    let obj = match settle(promise, deadline_ms).await {
        Settled::Value(v) => v,
        Settled::Error(_) => return Err(BodyError::Transport),
        Settled::Timeout => return Err(BodyError::Timeout),
    };
    if obj.is_null() || obj.is_undefined() {
        return Ok(None);
    }
    let size = Reflect::get(&obj, &JsValue::from_str("size")).ok().and_then(|v| v.as_f64()).ok_or(BodyError::Transport)?;
    if !(size >= 0.0 && size.fract() == 0.0) {
        return Err(BodyError::Transport);
    }
    let body = Reflect::get(&obj, &JsValue::from_str("body")).ok().filter(|v| !v.is_null() && !v.is_undefined());
    if size > bound as f64 {
        // Refused unread: the body stream is cancelled, never buffered.
        if let Some(body) = &body {
            let _ = call0(body, "cancel");
        }
        return Err(BodyError::TooLarge);
    }
    read(body, Length::Exact(size as usize), bound, deadline_ms).await.map(Some)
}

/// An R2 object's size from a `head` (no body is read), raced against
/// `deadline_ms` like [`r2_bytes`]. `Ok(None)`: no object.
pub async fn r2_size(bucket: &worker::Bucket, key: &str, deadline_ms: Option<u64>) -> Result<Option<u64>, BodyError> {
    let b: &JsValue = bucket.as_ref();
    let head: Function = Reflect::get(b, &JsValue::from_str("head"))
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or(BodyError::Transport)?;
    let promise: js_sys::Promise =
        head.call1(b, &JsValue::from_str(key)).ok().and_then(|p| p.dyn_into().ok()).ok_or(BodyError::Transport)?;
    let obj = match settle(promise, deadline_ms).await {
        Settled::Value(v) => v,
        Settled::Error(_) => return Err(BodyError::Transport),
        Settled::Timeout => return Err(BodyError::Timeout),
    };
    if obj.is_null() || obj.is_undefined() {
        return Ok(None);
    }
    let size = Reflect::get(&obj, &JsValue::from_str("size")).ok().and_then(|v| v.as_f64()).ok_or(BodyError::Transport)?;
    if !(size >= 0.0 && size.fract() == 0.0) {
        return Err(BodyError::Transport);
    }
    Ok(Some(size as u64))
}

/// The buffer [`r2_fold`] copies an object's body through: whatever the
/// object's length, the fold holds this much of it in linear memory.
pub const FOLD_BUFFER_BYTES: usize = 64 * 1024;

/// Feeds an R2 object's body to `f` through one [`FOLD_BUFFER_BYTES`]
/// buffer, chunk by chunk as the body stream delivers it, raced against
/// `deadline_ms` like [`r2_bytes`]. `Ok(false)`: no object. The body is
/// never held whole, so an object of any length is read in a fixed
/// footprint (a signed receipt index checked as it streams).
pub async fn r2_fold(
    bucket: &worker::Bucket,
    key: &str,
    deadline_ms: Option<u64>,
    f: &mut dyn FnMut(&[u8]),
) -> Result<bool, BodyError> {
    let b: &JsValue = bucket.as_ref();
    let get: Function = Reflect::get(b, &JsValue::from_str("get"))
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or(BodyError::Transport)?;
    let promise: js_sys::Promise =
        get.call1(b, &JsValue::from_str(key)).ok().and_then(|p| p.dyn_into().ok()).ok_or(BodyError::Transport)?;
    let obj = match settle(promise, deadline_ms).await {
        Settled::Value(v) => v,
        Settled::Error(_) => return Err(BodyError::Transport),
        Settled::Timeout => return Err(BodyError::Timeout),
    };
    if obj.is_null() || obj.is_undefined() {
        return Ok(false);
    }
    let Some(body) = Reflect::get(&obj, &JsValue::from_str("body")).ok().filter(|v| !v.is_null() && !v.is_undefined()) else {
        return Ok(true);
    };
    let reader = call0(&body, "getReader")?;
    let refuse = |e: BodyError| {
        let _ = call0(&reader, "cancel");
        e
    };
    let mut buf = vec![0u8; FOLD_BUFFER_BYTES];
    loop {
        let promise: js_sys::Promise = call0(&reader, "read")?.dyn_into().map_err(|_| BodyError::Transport)?;
        let step = match settle(promise, deadline_ms).await {
            Settled::Value(v) => v,
            Settled::Error(_) => return Err(BodyError::Transport),
            Settled::Timeout => return Err(refuse(BodyError::Timeout)),
        };
        if Reflect::get(&step, &JsValue::from_str("done")).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            return Ok(true);
        }
        let chunk: Uint8Array = Reflect::get(&step, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.dyn_into().ok())
            .ok_or(BodyError::Transport)
            .map_err(refuse)?;
        let len = chunk.length() as usize;
        let mut at = 0usize;
        while at < len {
            let n = (len - at).min(FOLD_BUFFER_BYTES);
            let Some(dst) = buf.get_mut(..n) else {
                return Err(refuse(BodyError::Transport));
            };
            chunk.subarray(at as u32, (at + n) as u32).copy_to(dst);
            f(dst);
            at += n;
        }
    }
}

/// One page of an R2 listing under `prefix` grouped by `delimiter`: its
/// delimited prefixes and, when the listing is truncated, the cursor that
/// continues it. Raced against `deadline_ms` like [`r2_bytes`]. A page holds
/// at most the platform's 1,000 entries, each an R2 key (at most 1,024
/// bytes), so it is bounded by the platform.
pub async fn r2_list_prefixes(
    bucket: &worker::Bucket,
    prefix: &str,
    delimiter: &str,
    cursor: Option<&str>,
    deadline_ms: Option<u64>,
) -> Result<(Vec<String>, Option<String>), BodyError> {
    let b: &JsValue = bucket.as_ref();
    let list: Function = Reflect::get(b, &JsValue::from_str("list"))
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or(BodyError::Transport)?;
    let options = js_sys::Object::new();
    let set = |k: &str, v: &str| Reflect::set(&options, &JsValue::from_str(k), &JsValue::from_str(v)).map(|_| ());
    set("prefix", prefix).map_err(|_| BodyError::Transport)?;
    set("delimiter", delimiter).map_err(|_| BodyError::Transport)?;
    if let Some(c) = cursor {
        set("cursor", c).map_err(|_| BodyError::Transport)?;
    }
    let promise: js_sys::Promise =
        list.call1(b, &options).ok().and_then(|p| p.dyn_into().ok()).ok_or(BodyError::Transport)?;
    let page = match settle(promise, deadline_ms).await {
        Settled::Value(v) => v,
        Settled::Error(_) => return Err(BodyError::Transport),
        Settled::Timeout => return Err(BodyError::Timeout),
    };
    let prefixes = Reflect::get(&page, &JsValue::from_str("delimitedPrefixes"))
        .ok()
        .and_then(|v| v.dyn_into::<js_sys::Array>().ok())
        .ok_or(BodyError::Transport)?;
    let mut out = Vec::with_capacity(prefixes.length() as usize);
    for p in prefixes.iter() {
        out.push(p.as_string().ok_or(BodyError::Transport)?);
    }
    let truncated = Reflect::get(&page, &JsValue::from_str("truncated")).ok().and_then(|v| v.as_bool()).unwrap_or(false);
    let next = if truncated {
        Some(Reflect::get(&page, &JsValue::from_str("cursor")).ok().and_then(|v| v.as_string()).ok_or(BodyError::Transport)?)
    } else {
        None
    };
    Ok((out, next))
}

/// Writes `bytes` to `key`, raced against `deadline_ms` in JS (`settle`):
/// `true` when the put completed in time. The bytes are copied into JS
/// before the call.
pub async fn r2_put(bucket: &worker::Bucket, key: &str, bytes: &[u8], deadline_ms: Option<u64>) -> bool {
    let b: &JsValue = bucket.as_ref();
    let Some(put) = Reflect::get(b, &JsValue::from_str("put")).ok().and_then(|f| f.dyn_into::<Function>().ok()) else {
        return false;
    };
    let body = Uint8Array::from(bytes);
    let Some(promise) = put.call2(b, &JsValue::from_str(key), &body).ok().and_then(|p| p.dyn_into::<js_sys::Promise>().ok()) else {
        return false;
    };
    matches!(settle(promise, deadline_ms).await, Settled::Value(_))
}

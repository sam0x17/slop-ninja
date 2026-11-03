//! Workers side of coordinator admission (wasm32 only): the public
//! `/admission/v1/` routes of the eval Worker, the `EvalCohort` object's
//! admission, template and commitment-reader routes, and its admission
//! steps (completions, framing, blob copies, verdict runs, clean-up, the
//! calendar).
//!
//! The routes are served only when the deployment opens them
//! (`ADMISSION_ROUTES`: `off`, the default, answers 404; `hotkeys` serves
//! only the hotkeys `ADMISSION_HOTKEYS` lists, for drills before public
//! access; `open` serves every hotkey). The Worker holds no state: it checks
//! each signed request's signature and body digest itself, so a request that
//! fails them never reaches the cohort authority, then forwards the signed
//! fields to the cohort object, which authenticates against the cursor,
//! decides and records in one transaction, then performs the R2 control
//! operations (create, abort, complete a multipart upload) and records
//! their result. A part's body never passes through Rust: the Worker hands
//! the request's own body stream to R2's `uploadPart` between the object's
//! `part-begin` and `part-record`. A completion is durable work until R2's
//! outcome is recorded. Framing streams the completed object chunk by chunk
//! through one fixed buffer; slices are copied object-to-blob as JS streams.
//! Nothing here parses the graph or the role files: the verdict is the
//! scorer Container's admission load.

use super::eval_do::{self, EVAL_BUCKET, EvalCohort, cohort_fetch, lane_stub};
use super::{TxError, error, json, now_ms, transaction};
use crate::eval::admission::{self, AdmissionEnv, Effects, Framer, LaneOutcome, PartTarget, Reply, Signed};
use crate::eval::{calendar, commitments, engine};
use crate::sql::Sql;
use js_sys::{Function, Reflect, Uint8Array};
use serde::Deserialize;
use serde_json::{Value, json as j};
use slop_ninja_admission_wire::{Route, UPLOAD_MEDIA_TYPE, header};
use slop_ninja_protocol::ChainBinding;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};
use worker::{Env, Method, Request, Response, Result};

/// A session-creation body: the fixed-size `SubmissionUpload` record, far
/// below this (the contract refuses a larger body with `bad_request`).
const UPLOAD_BODY_BYTES: usize = 4 * 1024;
/// A bodiless signed request's body bound (none is expected).
const EMPTY_BODY_BYTES: usize = 1024;
/// The bound of a cohort object's admission reply the Worker forwards: the
/// contract's response limit at R2's part maximum.
const REPLY_BYTES: usize = 64 * 1024 + 256 * crate::eval::plan::R2_MAX_PARTS as usize;
/// The fixed buffer framing streams an object through.
const FRAME_CHUNK_BYTES: usize = 1 << 20;
/// A commitment-reader round's request body: `commitments::plan` puts at
/// most one large read in a round, so its replies are one large reply and
/// the small ones beside it, which the reader's share bounds in number
/// (each small reply within the RPC client's default response bound).
pub const READER_BODY_BYTES: usize = 2 * crate::rpc::MAX_LARGE_RESPONSE_BYTES;

#[wasm_bindgen(inline_js = "
export function adm_fixed(body, len) {
  const f = new FixedLengthStream(len);
  body.pipeTo(f.writable).catch(() => {});
  return f.readable;
}
export function adm_get(bucket, key, offset, length) {
  const opts = (offset === undefined) ? undefined : { range: { offset: offset, length: length } };
  return bucket.get(key, opts);
}
")]
extern "C" {
    fn adm_fixed(body: &JsValue, len: f64) -> JsValue;
    fn adm_get(bucket: &JsValue, key: &str, offset: Option<f64>, length: Option<f64>) -> js_sys::Promise;
}

fn chain(env: &Env) -> Option<ChainBinding> {
    eval_do::eval_config(env, engine::INSTANCE_MAIN).ok().map(|c| c.chain)
}

fn reply_response(r: &Reply) -> Result<Response> {
    let mut resp = Response::from_json(&r.body)?.with_status(r.status);
    let h = resp.headers_mut();
    h.set("Cache-Control", "no-store")?;
    if let Some(s) = r.retry_after {
        h.set("Retry-After", &s.to_string())?;
    }
    Ok(resp)
}

fn api_error(code: slop_ninja_admission_wire::ErrorCode, message: &str) -> Result<Response> {
    reply_response(&Reply::error(&admission::ApiError::new(code, message)))
}

fn signed_of(req: &Request, path: &str) -> Result<Signed> {
    let h = req.headers();
    Ok(Signed {
        method: req.method().to_string(),
        path: path.to_owned(),
        hotkey: h.get(header::HOTKEY)?,
        valid_until: h.get(header::VALID_UNTIL)?,
        content_sha256: h.get(header::CONTENT_SHA256)?,
        signature: h.get(header::SIGNATURE)?,
        body_sha256: None,
        body: None,
    })
}

async fn to_cohort(env: &Env, op: &str, body: &Value) -> Result<Response> {
    let mut r = cohort_fetch(
        env,
        engine::INSTANCE_MAIN,
        Method::Post,
        &format!("/admission/{op}"),
        Some(body.to_string()),
    )
    .await?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return api_error(
            slop_ninja_admission_wire::ErrorCode::Unavailable,
            "the cohort authority is unavailable",
        );
    }
    match super::cf_io::response_json::<Reply>(&mut r, REPLY_BYTES).await {
        Ok(reply) => reply_response(&reply),
        Err(_) => api_error(slop_ninja_admission_wire::ErrorCode::Internal, "cohort reply"),
    }
}

/// Who the public routes serve (`ADMISSION_ROUTES`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Access {
    /// Absent or `off`: the routes do not exist (404).
    Off,
    /// Only the listed hotkeys (`ADMISSION_HOTKEYS`, SS58, comma separated):
    /// operator drills before public access.
    Hotkeys(Vec<String>),
    /// Every hotkey (public access, once decided).
    Open,
}

fn access(env: &Env) -> Access {
    match env
        .var("ADMISSION_ROUTES")
        .map(|v| v.to_string())
        .unwrap_or_default()
        .trim()
    {
        "open" => Access::Open,
        "hotkeys" => Access::Hotkeys(
            env.var("ADMISSION_HOTKEYS")
                .map(|v| v.to_string())
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        _ => Access::Off,
    }
}

/// The stateless request checks, in the Worker before anything is
/// forwarded: the signature and body digest verify, and in `hotkeys` mode
/// the signer is listed (an unlisted one sees no route).
fn admit_request(access: &Access, chain: &ChainBinding, signed: &Signed) -> std::result::Result<(), Reply> {
    let v = admission::verify_request(chain, signed).map_err(|e| Reply::error(&e))?;
    if let Access::Hotkeys(list) = access
        && !list.iter().any(|h| *h == crate::chain::ss58(v.signer.0))
    {
        return Err(Reply::error(&admission::ApiError::new(
            slop_ninja_admission_wire::ErrorCode::NotFound,
            "no such route",
        )));
    }
    Ok(())
}

/// The public admission routes (eval Worker, main instance only).
pub async fn route(mut req: Request, env: Env) -> Result<Response> {
    use slop_ninja_admission_wire::ErrorCode as E;
    let access = access(&env);
    if access == Access::Off || !eval_do::enabled(&env) {
        return error("not_found", 404);
    }
    let Some(chain_binding) = chain(&env) else {
        return error("not_found", 404);
    };
    let path = req.path();
    let method = req.method().to_string();
    let Some(route) = Route::parse(&method, &path) else {
        return api_error(E::BadRequest, "no such route or method");
    };
    let mut signed = signed_of(&req, &path)?;
    // Every route but the unsigned cohort read is checked here first; the
    // body digest of a body read whole is set before the check.
    let check = |signed: &Signed| admit_request(&access, &chain_binding, signed);
    match route {
        Route::ReadCohort => {
            let mut r = cohort_fetch(&env, engine::INSTANCE_MAIN, Method::Get, "/admission/cohort", None).await?;
            if r.status_code() != 200 {
                super::cf_io::discard(&mut r);
                return api_error(E::Unavailable, "the cohort authority is unavailable");
            }
            match super::cf_io::response_json::<Value>(&mut r, REPLY_BYTES).await {
                Ok(v) => {
                    let mut resp = Response::from_json(&v)?;
                    resp.headers_mut().set("Cache-Control", "no-store")?;
                    Ok(resp)
                }
                Err(_) => api_error(E::Internal, "cohort reply"),
            }
        }
        Route::CreateSession => {
            let media = req.headers().get("content-type")?.unwrap_or_default();
            if media.split(';').next().map(str::trim) != Some(UPLOAD_MEDIA_TYPE) {
                return api_error(E::UnsupportedMediaType, "the body is not a submission-upload record");
            }
            let body = match super::cf_io::request_bytes(&mut req, UPLOAD_BODY_BYTES).await {
                Ok(b) => b,
                Err(super::cf_io::BodyError::TooLarge) => {
                    return api_error(E::BadRequest, "the body is larger than a submission-upload record");
                }
                Err(_) => return api_error(E::BadRequest, "body"),
            };
            signed.body_sha256 = Some(eval_do::sha_hex(&body));
            if let Err(r) = check(&signed) {
                return reply_response(&r);
            }
            signed.body = Some(hex::encode(&body));
            to_cohort(&env, "create", &j!({"signed": signed})).await
        }
        Route::PutPart { session, part } => {
            if let Err(r) = check(&signed) {
                return reply_response(&r);
            }
            put_part(&mut req, &env, signed, session, part).await
        }
        Route::ReadSession { session } => {
            signed.body_sha256 = Some(eval_do::sha_hex(&[]));
            if let Err(r) = check(&signed) {
                return reply_response(&r);
            }
            to_cohort(&env, "read", &j!({"signed": signed, "session": session})).await
        }
        Route::Complete { session } | Route::Abort { session, .. } | Route::Reopen { session, .. } => {
            let body = match super::cf_io::request_bytes(&mut req, EMPTY_BODY_BYTES).await {
                Ok(b) => b,
                Err(_) => return api_error(E::BadRequest, "these routes take an empty body"),
            };
            signed.body_sha256 = Some(eval_do::sha_hex(&body));
            if let Err(r) = check(&signed) {
                return reply_response(&r);
            }
            let (op, attempt) = match route {
                Route::Complete { .. } => ("complete", None),
                Route::Abort { attempt, .. } => ("abort", Some(attempt)),
                Route::Reopen { attempt, .. } => ("reopen", Some(attempt)),
                _ => ("read", None),
            };
            to_cohort(
                &env,
                op,
                &j!({"signed": signed, "session": session, "attempt": attempt}),
            )
            .await
        }
        Route::ReadHotkey { cohort, hotkey } => {
            signed.body_sha256 = Some(eval_do::sha_hex(&[]));
            if let Err(r) = check(&signed) {
                return reply_response(&r);
            }
            to_cohort(
                &env,
                "hotkey",
                &j!({"signed": signed, "cohort": cohort, "hotkey": hotkey}),
            )
            .await
        }
    }
}

/// `PUT .../parts/{n}`: checked by the cohort object, streamed by R2 from
/// the request's own body, then recorded by the cohort object.
async fn put_part(req: &mut Request, env: &Env, signed: Signed, session: &str, part: u32) -> Result<Response> {
    use slop_ninja_admission_wire::ErrorCode as E;
    let declared = req
        .headers()
        .get("content-length")?
        .and_then(|v| v.trim().parse::<u64>().ok());
    let mut r = cohort_fetch(
        env,
        engine::INSTANCE_MAIN,
        Method::Post,
        "/admission/part-begin",
        Some(j!({"signed": signed, "session": session, "part": part, "content_length": declared}).to_string()),
    )
    .await?;
    if r.status_code() != 200 {
        super::cf_io::discard(&mut r);
        return api_error(E::Unavailable, "the cohort authority is unavailable");
    }
    #[derive(Deserialize)]
    struct Begin {
        target: Option<PartTarget>,
        reply: Option<Reply>,
    }
    let begin = match super::cf_io::response_json::<Begin>(&mut r, REPLY_BYTES).await {
        Ok(b) => b,
        Err(_) => return api_error(E::Internal, "cohort reply"),
    };
    let Some(t) = begin.target else {
        return match begin.reply {
            Some(reply) => reply_response(&reply),
            None => api_error(E::Internal, "cohort reply"),
        };
    };
    let Some(stream) = req.inner().body() else {
        return api_error(E::PartLengthMismatch, "the part has no body");
    };
    let Ok(n) = u16::try_from(t.part) else {
        return api_error(E::PartOutOfRange, "no such part");
    };
    let bucket = env.bucket(EVAL_BUCKET)?;
    let etag = match bucket.resume_multipart_upload(t.key.clone(), t.upload_id.clone()) {
        Ok(upload) => match upload.upload_part(n, worker::Data::ReadableStream(stream)).await {
            Ok(p) => p.etag(),
            Err(_) => {
                let mut e = admission::ApiError::new(E::Unavailable, "the part could not be stored; send it again");
                e.retry_after = Some(2);
                return reply_response(&Reply::error(&e));
            }
        },
        Err(_) => return api_error(E::Unavailable, "upload"),
    };
    to_cohort(env, "part-record", &j!({"session": session, "target": t, "etag": etag})).await
}

// ------------------------------------------------------- cohort object side

#[derive(Deserialize)]
struct AdmissionBody {
    #[serde(default)]
    signed: Signed,
    #[serde(default)]
    session: String,
    #[serde(default)]
    attempt: Option<u32>,
    #[serde(default)]
    part: u32,
    #[serde(default)]
    content_length: Option<u64>,
    #[serde(default)]
    cohort: u64,
    #[serde(default)]
    hotkey: String,
    #[serde(default)]
    target: Option<PartTarget>,
    #[serde(default)]
    etag: String,
}

fn tx_or_unavailable<T>(r: std::result::Result<T, TxError<engine::EvalError>>) -> std::result::Result<T, Reply> {
    r.map_err(|_| {
        let mut e = admission::ApiError::new(slop_ninja_admission_wire::ErrorCode::Unavailable, "store");
        e.retry_after = Some(2);
        Reply::error(&e)
    })
}

impl EvalCohort {
    /// The cohort object's `/admission/*` routes (from the Worker only).
    pub(super) async fn admission_route(&self, req: &mut Request, path: &str) -> Result<Response> {
        let Some(chain) = chain(&self.env) else {
            return error("not_configured", 503);
        };
        let storage = self.state.storage();
        if path == "/admission/cohort" {
            let paused = !self.fresh();
            return match transaction(&storage, move |sql| admission::cohort_info(sql, &chain, paused)) {
                Ok(v) => json(&v),
                Err(_) => error("store_unavailable", 503),
            };
        }
        let b: AdmissionBody = match super::cf_io::request_json(req, admission_body_bytes()).await {
            Ok(b) => b,
            Err(e) => return error(e.code(), e.status()),
        };
        let now = now_ms();
        let reply = match path {
            "/admission/create" => {
                let c2 = chain.clone();
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(admission::create(sql, &c2, &b.signed, now))
                });
                match tx_or_unavailable(r) {
                    Ok((reply, fx)) => {
                        self.effects(fx).await;
                        reply
                    }
                    Err(reply) => reply,
                }
            }
            "/admission/read" => {
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(admission::read(sql, &chain, &b.signed, &b.session))
                });
                tx_or_unavailable(r).unwrap_or_else(|e| e)
            }
            "/admission/part-begin" => {
                // A session whose multipart upload is not yet created gets it now.
                let id = b.session.clone();
                if let Ok(Some((key, attempt))) = transaction(&storage, move |sql| admission::upload_missing(sql, &id))
                {
                    self.effects(Effects {
                        create: Some((key, b.session.clone(), attempt)),
                        ..Default::default()
                    })
                    .await;
                }
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(admission::part_begin(
                        sql,
                        &chain,
                        &b.signed,
                        &b.session,
                        b.part,
                        b.content_length,
                    ))
                });
                return match tx_or_unavailable(r) {
                    Ok(Ok(t)) => json(&j!({"target": t})),
                    Ok(Err(reply)) | Err(reply) => json(&j!({"reply": reply})),
                };
            }
            "/admission/part-record" => {
                let Some(t) = b.target else {
                    return error("bad_internal_request", 400);
                };
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(admission::part_record(sql, &b.session, &t, &b.etag))
                });
                tx_or_unavailable(r).unwrap_or_else(|e| e)
            }
            "/admission/complete" => {
                let id = b.session.clone();
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(admission::complete(sql, &chain, &b.signed, &b.session, now))
                });
                match tx_or_unavailable(r) {
                    Ok((reply, fx)) => {
                        // The completion is durable work from here: run once
                        // now, and resumed by the cohort's step until R2's
                        // outcome is recorded.
                        if fx.complete.is_some() {
                            let _ = self.complete_step(&id).await;
                        }
                        let _ = storage.set_alarm(500i64).await;
                        reply
                    }
                    Err(reply) => reply,
                }
            }
            "/admission/abort" | "/admission/reopen" => {
                let attempt = b.attempt.unwrap_or(0);
                let abort = path == "/admission/abort";
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(if abort {
                        admission::abort(sql, &chain, &b.signed, &b.session, attempt, now)
                    } else {
                        admission::reopen(sql, &chain, &b.signed, &b.session, attempt, now)
                    })
                });
                match tx_or_unavailable(r) {
                    Ok((reply, fx)) => {
                        self.effects(fx).await;
                        reply
                    }
                    Err(reply) => reply,
                }
            }
            "/admission/hotkey" => {
                let r = transaction(&storage, move |sql| {
                    Ok::<_, engine::EvalError>(admission::hotkey_view(sql, &chain, &b.signed, b.cohort, &b.hotkey))
                });
                tx_or_unavailable(r).unwrap_or_else(|e| e)
            }
            _ => return error("not_found", 404),
        };
        json(&reply)
    }

    /// Whether the cohort object's heartbeat is fresh (the coordinator is
    /// observing); admission answers `paused` in the cohort response when not.
    fn fresh(&self) -> bool {
        let storage = self.state.storage();
        transaction(&storage, engine::observed)
            .map(|o| now_ms().saturating_sub(o.kick_ms) < engine::HEARTBEAT_FRESH_MS)
            .unwrap_or(false)
    }

    /// Performs a session operation's R2 control effects after its
    /// transaction committed, then records their results.
    async fn effects(&self, fx: Effects) {
        let Ok(bucket) = self.env.bucket(EVAL_BUCKET) else {
            return;
        };
        for (key, upload_id) in fx.abort {
            if let Some(id) = upload_id
                && let Ok(u) = bucket.resume_multipart_upload(key.clone(), id)
            {
                let _ = u.abort().await;
            }
            let _ = bucket.delete(key).await;
        }
        if let Some((key, session, attempt)) = fx.create
            && let Ok(u) = bucket.create_multipart_upload(key).execute().await
        {
            let id = u.upload_id().await;
            let storage = self.state.storage();
            let _ = transaction(&storage, move |sql| {
                admission::set_upload_id(sql, &session, attempt, &id)
            });
        }
    }

    /// One completion step of a session in stage `complete`: completes its
    /// multipart upload. R2's success, or the object found at the envelope's
    /// length (a completion that succeeded before its outcome was recorded),
    /// moves it to framing; a definitive refusal (a part that changed or went
    /// missing, an upload gone with no object) ends it with
    /// `object_mismatch`; anything else is retried after a backoff.
    async fn complete_step(&self, id: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let sid = id.to_owned();
        let Some(((key, upload_id, parts), len)) =
            transaction(&storage, move |sql| admission::completion_begin(sql, &sid, now)).map_err(|_| "completion")?
        else {
            return Ok(());
        };
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        let refused = match bucket.resume_multipart_upload(key.clone(), upload_id) {
            Ok(u) => match u
                .complete(parts.into_iter().map(|(n, etag)| worker::UploadedPart::new(n, etag)))
                .await
            {
                Ok(_) => Some(false),
                Err(e) => admission::completion_refused(&e.to_string()).then_some(true),
            },
            Err(e) => admission::completion_refused(&e.to_string()).then_some(true),
        };
        // `Some(false)`: completed. Otherwise the object decides: present at
        // the envelope's length, the upload completed earlier.
        let outcome = match refused {
            Some(false) => Some(true),
            other => match bucket.head(key).await {
                Ok(Some(o)) if o.size() == len => Some(true),
                Ok(_) if other == Some(true) => Some(false),
                _ => None,
            },
        };
        let now = now_ms();
        let id = id.to_owned();
        transaction(&storage, move |sql| match outcome {
            Some(true) => admission::completed(sql, &id, now),
            Some(false) => admission::completion_failed(sql, &id, now),
            None => admission::step_retry(sql, &id, false, now),
        })
        .map_err(|_| "completion commit")
    }

    /// The cohort object's `/template`, `/template/pause` and `/calendar`
    /// operator routes.
    pub(super) async fn template_route(
        &self,
        req: &mut Request,
        path: &str,
        cfg: engine::EvalConfig,
    ) -> Result<Response> {
        let storage = self.state.storage();
        let now = now_ms();
        match (req.method(), path) {
            (Method::Post, "/template") => {
                let bytes = match super::cf_io::request_bytes(req, crate::arena_routes::MAX_PLAN_BODY_BYTES).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| calendar::install(sql, &cfg, &bytes, now)) {
                    Ok(v) => {
                        storage.set_alarm(500i64).await?;
                        json(&v)
                    }
                    Err(TxError::Operation(e)) => {
                        let mut resp = Response::from_json(&j!({"error": e.code()}))?.with_status(422);
                        resp.headers_mut().set("Cache-Control", "no-store")?;
                        Ok(resp)
                    }
                    Err(TxError::Runtime) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/template/pause") => {
                let v: Value = super::cf_io::request_json(req, 1024).await.unwrap_or(Value::Null);
                let paused = v.get("paused") != Some(&Value::Bool(false));
                match transaction(&storage, move |sql| calendar::set_paused(sql, paused)) {
                    Ok(found) => json(&j!({"template": found, "paused": paused})),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            (Method::Post, "/session/refuse") => {
                // The operator's recorded way out of a stalled session.
                #[derive(Deserialize)]
                struct B {
                    session: String,
                    reason: String,
                }
                let b: B = match super::cf_io::request_json(req, 1024).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                match transaction(&storage, move |sql| {
                    admission::operator_refuse(sql, &b.session, &b.reason, now)
                }) {
                    Ok(refused) => {
                        storage.set_alarm(500i64).await?;
                        json(&j!({"refused": refused}))
                    }
                    Err(TxError::Operation(e)) => error(e.code(), 400),
                    Err(TxError::Runtime) => error("store_unavailable", 503),
                }
            }
            (Method::Get, "/calendar") => match transaction(&storage, calendar::view) {
                Ok(v) => json(&v),
                Err(_) => error("store_unavailable", 503),
            },
            (Method::Get, "/cost-model") => {
                // The latest cohort's cost model, else the template's.
                let r = transaction(&storage, |sql| {
                    let last = engine::cohorts(sql)?
                        .into_iter()
                        .rev()
                        .find_map(|c| c.plan.fanout.map(|f| f.cost));
                    Ok::<_, engine::EvalError>(match last {
                        Some(m) => Some(m),
                        None => calendar::template(sql)?.and_then(|p| p.fanout.map(|f| f.cost)),
                    })
                });
                match r {
                    Ok(Some(m)) => json(&m),
                    Ok(None) => error("no_cost_model", 404),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            _ => error("not_found", 404),
        }
    }

    /// The cohort object's commitment-reader routes (from the Worker's cron).
    pub(super) async fn commitments_route(&self, req: &mut Request, path: &str) -> Result<Response> {
        let Some(chain) = chain(&self.env) else {
            return error("not_configured", 503);
        };
        let storage = self.state.storage();
        let netuid = chain.netuid;
        let now = now_ms();
        match path {
            "/commitments/plan" => {
                #[derive(Deserialize)]
                struct B {
                    /// The reader's share of the RPC budget, calls a minute.
                    per_minute: u64,
                }
                let b: B = match super::cf_io::request_json(req, 1024).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                let r = transaction(&storage, move |sql| {
                    let o = engine::observed(sql)?;
                    let needs = commitments::Needs {
                        poll: admission::window(sql)?.is_some_and(|w| w.admitting()),
                        snapshot: admission::snapshot_needs(sql)?,
                    };
                    let rt = runtime(sql)?;
                    commitments::plan(sql, rt.as_ref(), netuid, o.confirmed, &needs, b.per_minute, now)
                });
                match r {
                    Ok(round) => json(&round),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/commitments/ingest" => {
                #[derive(Deserialize)]
                struct B {
                    results: Vec<commitments::ReadResult>,
                }
                let b: B = match super::cf_io::request_json(req, READER_BODY_BYTES).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                let r = transaction(&storage, move |sql| {
                    let rt = runtime(sql)?;
                    let out = commitments::ingest(sql, rt.as_ref(), netuid, &b.results, now)?;
                    if let Some((spec, tx, meta, at, marker)) = &out.runtime {
                        // Stored only when it parses as V15 metadata.
                        if crate::chain::ChainRuntime::parse(meta, *spec, *tx).is_ok() {
                            commitments::store_runtime(sql, *spec, *tx, meta, *at, marker)?;
                        }
                    }
                    Ok::<_, engine::EvalError>(out)
                });
                match r {
                    Ok(out) => json(&out),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            _ => error("not_found", 404),
        }
    }

    // ------------------------------------------------------------- steps

    /// One admission step of the alarm.
    pub(super) async fn admission_step(
        &self,
        work: engine::Work,
        cfg: &engine::EvalConfig,
    ) -> std::result::Result<(), &'static str> {
        match work {
            engine::Work::AdmissionVerify { session, stage } if stage == "complete" => {
                self.complete_step(&session).await
            }
            engine::Work::AdmissionVerify { session, stage } if stage == "frame" => self.frame(&session).await,
            engine::Work::AdmissionVerify { session, .. } => self.blobs(&session).await,
            engine::Work::AdmissionLanes => self.lanes().await,
            engine::Work::AdmissionCleanup => self.cleanup().await,
            engine::Work::OpenNext { label, plan } => self.open_next(cfg, &label, plan).await,
            _ => Ok(()),
        }
    }

    async fn frame(&self, session: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let id = session.to_owned();
        let now = now_ms();
        let Some((key, len, version)) =
            transaction(&storage, move |sql| admission::frame_begin(sql, &id, now)).map_err(|_| "frame target")?
        else {
            return Ok(());
        };
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        let mut framer = Framer::new();
        let read = stream_object(&bucket, &key, None, |chunk| framer.update(chunk)).await;
        let framed = match read {
            Ok(Some(n)) if n == len => framer.finish(version),
            // A missing object or a length the envelope does not name.
            Ok(_) => framer.finish(version),
            Err(e) => {
                // A read that failed is the platform's: not counted.
                let id = session.to_owned();
                let now = now_ms();
                let _ = transaction(&storage, move |sql| admission::step_retry(sql, &id, true, now));
                return Err(e);
            }
        };
        let chain = chain(&self.env).ok_or("chain")?;
        let id = session.to_owned();
        let now = now_ms();
        transaction(&storage, move |sql| {
            admission::apply_frame(sql, &chain, &id, framed, now)
        })
        .map_err(|_| "frame commit")
    }

    /// The blob copies of one session; a failure backs that session off, so
    /// other sessions' steps are not held behind it.
    async fn blobs(&self, session: &str) -> std::result::Result<(), &'static str> {
        let r = self.blobs_once(session).await;
        if r.is_err() {
            let storage = self.state.storage();
            let (id, now) = (session.to_owned(), now_ms());
            let _ = transaction(&storage, move |sql| admission::step_retry(sql, &id, false, now));
        }
        r
    }

    async fn blobs_once(&self, session: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let id = session.to_owned();
        let Some((key, rec)) =
            transaction(&storage, move |sql| admission::frame_record(sql, &id)).map_err(|_| "frame record")?
        else {
            return Ok(());
        };
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        // The slices this session's copies create (the store held none), so
        // its clean-up deletes exactly those unless another session uses them.
        // Each creation is recorded before the next copy, so a step cut off
        // half way still knows what it created.
        for sl in &rec.slices {
            let blob = format!("artifacts/blobs/{}", sl.sha256);
            if bucket.head(blob.clone()).await.map_err(|_| "blob head")?.is_some() {
                continue;
            }
            let (id, sha) = (session.to_owned(), sl.sha256.clone());
            transaction(&storage, move |sql| admission::blob_created(sql, &id, &sha)).map_err(|_| "blob record")?;
            copy_range(&bucket, &key, sl.offset, sl.len, &blob).await?;
        }
        if bucket
            .head(rec.manifest_key.clone())
            .await
            .map_err(|_| "manifest head")?
            .is_none()
        {
            let id = session.to_owned();
            transaction(&storage, move |sql| admission::manifest_created(sql, &id)).map_err(|_| "manifest record")?;
        }
        let manifest = hex::decode(&rec.manifest_hex).map_err(|_| "manifest hex")?;
        eval_do::put(&self.env, EVAL_BUCKET, &rec.manifest_key, manifest)
            .await
            .map_err(|_| "manifest put")?;
        let env = admission_env(&self.env);
        let id = session.to_owned();
        let now = now_ms();
        transaction(&storage, move |sql| admission::blobs_written(sql, &env, &id, now)).map_err(|_| "blobs commit")
    }

    /// Runs the claimable verdicts on fresh scorer instances, at most
    /// [`eval_do::PARALLEL`] at once and never past the deployment's ceiling.
    async fn lanes(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let env = admission_env(&self.env);
        let e2 = env.clone();
        let now = now_ms();
        let runs = transaction(&storage, move |sql| {
            admission::claim_runs(sql, &e2, eval_do::PARALLEL, now)
        })
        .map_err(|_| "claim runs")?;
        let outcomes = futures_util::future::join_all(runs.iter().map(|run| async move {
            let name = run.lane_name(crate::config::NETWORK);
            let body = j!({"package_sha256": run.package_sha256, "manifest_key": run.manifest_key,
                "docs": [{"id": "0", "text": crate::eval::lane_logic::CONTROL_TEXT}]});
            let t0 = now_ms();
            let outcome = async {
                // The probe route admits by spec v2 (a load that names no
                // spec); plans under another spec are refused at `/plan`
                // and `/template` (`admission::ADMISSION_SPECS`).
                let v2 = slop_ninja_artifact_check::spec::Version::V2.spec_sha256();
                if run.spec_sha256 != v2 {
                    return LaneOutcome::Platform("admission_spec_unsupported".into());
                }
                let Ok(stub) = lane_stub(&self.env, "score", &name) else {
                    return LaneOutcome::Platform("lane_binding".into());
                };
                let Ok(req) = eval_do::internal_request(Method::Post, "/run", Some(body.to_string())) else {
                    return LaneOutcome::Platform("lane_request".into());
                };
                match stub.fetch_with_request(req).await {
                    Ok(mut r) => {
                        let status = r.status_code();
                        let v: Value = super::cf_io::response_json(&mut r, 64 * 1024)
                            .await
                            .unwrap_or(Value::Null);
                        admission::classify(Some(status), &v)
                    }
                    Err(_) => admission::classify(None, &Value::Null),
                }
            }
            .await;
            // The instance served one artifact: stop it now.
            if let (Ok(stub), Ok(req)) = (
                lane_stub(&self.env, "score", &name),
                eval_do::internal_request(Method::Post, "/stop", None),
            ) {
                let _ = stub.fetch_with_request(req).await;
            }
            (outcome, now_ms().saturating_sub(t0))
        }))
        .await;
        // Each run's instance time, at the cohort's Container rates, is
        // settled in the tracking ledger under its own key (never refused).
        for (run, (outcome, wall_ms)) in runs.into_iter().zip(outcomes) {
            let e2 = env.clone();
            let now = now_ms();
            let label = run.label.clone();
            let cost = transaction(&storage, move |sql| {
                let bound = admission::failures_bound(sql, &run.label)?;
                admission::record_run(sql, &e2, &run, &outcome, bound, now)?;
                admission::run_cost_model(sql, &run.label)
            })
            .map_err(|_| "record run")?;
            if let Some(model) = cost {
                let micro = crate::eval::cost::probe_charge_microusd(&model, wall_ms);
                if micro > 0 {
                    let key = format!("{}:{label}:admission:{now}", engine::INSTANCE_MAIN);
                    let _ = eval_do::ledger_call(
                        &self.env,
                        "/settle",
                        &j!({"key": key, "microusd": micro, "note": "admission run"}),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn cleanup(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let due = transaction(&storage, |sql| admission::cleanup_due(sql, eval_do::PARALLEL as i64))
            .map_err(|_| "cleanup due")?;
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        for (id, key, upload_id) in due {
            if let Some(u) = upload_id
                && let Ok(m) = bucket.resume_multipart_upload(key.clone(), u)
            {
                let _ = m.abort().await;
            }
            bucket.delete(key).await.map_err(|_| "object delete")?;
            // The blobs and manifest only this session's copies created.
            let sid = id.clone();
            let extra =
                transaction(&storage, move |sql| admission::cleanup_keys(sql, &sid)).map_err(|_| "cleanup keys")?;
            for k in extra {
                bucket.delete(k).await.map_err(|_| "blob delete")?;
            }
            transaction(&storage, move |sql| admission::mark_cleaned(sql, &id)).map_err(|_| "cleanup commit")?;
        }
        Ok(())
    }

    /// Queues the next cohort from the standing template. Its budgets are
    /// recorded in the tracking ledger (never refused); a plan the engine
    /// refuses pauses the template with the reason, for the operator.
    async fn open_next(
        &self,
        cfg: &engine::EvalConfig,
        label: &str,
        plan: Vec<u8>,
    ) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let adm = match engine::admit(cfg, &plan) {
            Ok((_, adm)) => adm,
            Err(e) => {
                let code = e.code();
                let _ = transaction(&storage, move |sql| {
                    calendar::note_error(sql, code)?;
                    calendar::set_paused(sql, true).map(|_| ())
                });
                return Err("template plan refused");
            }
        };
        let key = format!("{}:{label}", engine::INSTANCE_MAIN);
        let _ = eval_do::ledger_call(
            &self.env,
            "/reserve",
            &j!({"key": key, "microusd": adm.reserve_microusd}),
        )
        .await;
        let c2 = cfg.clone();
        let now = now_ms();
        match transaction(&storage, move |sql| engine::queue(sql, &c2, &plan, now)) {
            Ok(_) => Ok(()),
            Err(TxError::Operation(e)) => {
                let code = e.code();
                let _ = transaction(&storage, move |sql| {
                    calendar::note_error(sql, code)?;
                    calendar::set_paused(sql, true).map(|_| ())
                });
                Err("template queue refused")
            }
            Err(TxError::Runtime) => Err("template queue"),
        }
    }
}

/// The stored reader runtime, parsed (`None` until one is stored).
fn runtime(sql: &impl Sql) -> std::result::Result<Option<crate::chain::ChainRuntime>, engine::EvalError> {
    Ok(commitments::load_runtime(sql)?
        .and_then(|(spec, tx, meta)| crate::chain::ChainRuntime::parse(&meta, spec, tx).ok()))
}

/// The deployment facts verdicts bind to.
pub fn admission_env(env: &Env) -> AdmissionEnv {
    AdmissionEnv {
        scorer_binary: env
            .var("SCORER_BINARY_SHA256")
            .map(|v| v.to_string())
            .unwrap_or_default(),
        max_instances: env
            .var("MAX_SCORE_LANES")
            .ok()
            .and_then(|v| v.to_string().trim().parse().ok())
            .unwrap_or(0),
    }
}

/// The admission request body bound: a signed request's fields and at most
/// a session-creation body (hex).
fn admission_body_bytes() -> usize {
    8 * 1024 + 2 * UPLOAD_BODY_BYTES
}

fn call0(target: &JsValue, name: &str) -> std::result::Result<JsValue, &'static str> {
    let f = Reflect::get(target, &JsValue::from_str(name)).map_err(|_| "js")?;
    let f: Function = f.dyn_into().map_err(|_| "js")?;
    f.call0(target).map_err(|_| "js")
}

/// Streams an R2 object (or a range of it) through `each`, one fixed buffer
/// at a time, so no read holds more than [`FRAME_CHUNK_BYTES`]. Returns the
/// bytes read, or `None` when the object is missing.
async fn stream_object(
    bucket: &worker::Bucket,
    key: &str,
    range: Option<(u64, u64)>,
    mut each: impl FnMut(&[u8]),
) -> std::result::Result<Option<u64>, &'static str> {
    let b: &JsValue = bucket.as_ref();
    let promise = adm_get(b, key, range.map(|r| r.0 as f64), range.map(|r| r.1 as f64));
    let obj = match super::cf_io::settle(promise, None).await {
        super::cf_io::Settled::Value(v) => v,
        _ => return Err("object read"),
    };
    if obj.is_null() || obj.is_undefined() {
        return Ok(None);
    }
    let body = Reflect::get(&obj, &JsValue::from_str("body")).map_err(|_| "object body")?;
    let reader = call0(&body, "getReader")?;
    let mut buf = vec![0u8; FRAME_CHUNK_BYTES];
    let mut total = 0u64;
    loop {
        let promise: js_sys::Promise = call0(&reader, "read")?.dyn_into().map_err(|_| "read")?;
        let step = match super::cf_io::settle(promise, None).await {
            super::cf_io::Settled::Value(v) => v,
            _ => return Err("object stream"),
        };
        if Reflect::get(&step, &JsValue::from_str("done"))
            .ok()
            .and_then(|d| d.as_bool())
            .unwrap_or(true)
        {
            break;
        }
        let chunk: Uint8Array = Reflect::get(&step, &JsValue::from_str("value"))
            .ok()
            .and_then(|v| v.dyn_into().ok())
            .ok_or("chunk")?;
        let n = chunk.length();
        let mut at = 0u32;
        while at < n {
            let take = (n - at).min(FRAME_CHUNK_BYTES as u32);
            let dst = buf.get_mut(..take as usize).ok_or("buffer")?;
            chunk.subarray(at, at + take).copy_to(dst);
            each(dst);
            at += take;
        }
        total += u64::from(n);
    }
    Ok(Some(total))
}

/// Copies `len` bytes at `offset` of `key` to `dest` as a JS stream (no byte
/// enters Rust).
async fn copy_range(
    bucket: &worker::Bucket,
    key: &str,
    offset: u64,
    len: u64,
    dest: &str,
) -> std::result::Result<(), &'static str> {
    let b: &JsValue = bucket.as_ref();
    let obj = match super::cf_io::settle(adm_get(b, key, Some(offset as f64), Some(len as f64)), None).await {
        super::cf_io::Settled::Value(v) if !v.is_null() && !v.is_undefined() => v,
        _ => return Err("slice read"),
    };
    let body = Reflect::get(&obj, &JsValue::from_str("body")).map_err(|_| "slice body")?;
    let fixed = adm_fixed(&body, len as f64);
    let put: Function = Reflect::get(b, &JsValue::from_str("put"))
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or("put")?;
    let promise: js_sys::Promise = put
        .call2(b, &JsValue::from_str(dest), &fixed)
        .ok()
        .and_then(|p| p.dyn_into().ok())
        .ok_or("put")?;
    match super::cf_io::settle(promise, None).await {
        super::cf_io::Settled::Value(_) => Ok(()),
        _ => Err("slice put"),
    }
}

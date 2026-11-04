//! Workers side of coordinator admission (wasm32 only): the eval Worker's
//! internal admission interface (reachable only through the upload Worker's
//! service binding), the `EvalCohort` object's admission, template and
//! commitment-reader routes, and its admission steps (completions, framing
//! through the upload Worker, blob copies, verdict runs, clean-up, the
//! calendar).
//!
//! The eval Worker serves no public admission route: every `/admission/`
//! request to it answers 404. The unsigned cohort read never reaches it:
//! at every heartbeat the cohort object writes the cohort response to the
//! upload bucket (`internal::COHORT_SNAPSHOT_KEY`), which the upload Worker
//! serves. The public `/admission/v1/` routes are the
//! key-free upload Worker's (`subnet/admission-worker`,
//! `slop-ninja-admission-testnet`): it decodes and checks every untrusted
//! request (signature, body digest, registration on the subnet, the
//! spec-derived size bound) and forwards only checked, bounded messages
//! here, to `/internal/admission/v1/<op>` on the internal origin, with the
//! shared interface token (`slop_ninja_admission_wire::internal`). This
//! Worker forwards them to the cohort object, which authenticates against
//! the cursor, decides and records in one transaction, then performs the R2
//! control operations (create, abort, complete a multipart upload, in the
//! upload bucket) and records their result. A part's body never reaches
//! this Worker: the upload Worker streams it to R2's `uploadPart` between
//! the object's `part-begin` and `part-record`. A completion is durable
//! work until R2's outcome is recorded. Framing (the one decode of an
//! uploaded object: its header record) runs in the upload Worker, which the
//! cohort object's framing step calls through this Worker's own service
//! binding; slices are then copied from the upload bucket to the eval
//! bucket as JS streams. Nothing here decodes an uploaded object, parses the
//! graph or the role files: the verdict is the scorer Container's
//! admission load.

use super::eval_do::{self, EVAL_BUCKET, EvalCohort, cohort_fetch, lane_stub};
use super::{TxError, error, json, now_ms, transaction};
use crate::eval::admission::{self, AdmissionEnv, Effects, FrameFailure, FrameOutcome, LaneOutcome, Reply};
use crate::eval::{calendar, commitments, engine};
use crate::sql::Sql;
use js_sys::{Function, Reflect};
use serde::Deserialize;
use serde_json::{Value, json as j};
use slop_ninja_admission_wire::internal::{self as iface, Forward};
use slop_ninja_protocol::ChainBinding;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};
use worker::{Env, Method, Request, Response, Result};

/// The upload bucket's binding: uploaded objects only (private; the upload
/// Worker binds the same bucket and nothing else).
pub const ADMISSION_BUCKET: &str = "ADMISSION_TESTNET";
/// The upload Worker's service binding (framing).
pub const UPLOAD_SERVICE: &str = "ADMISSION_UPLOAD_TESTNET";
/// The interface token, a Worker secret of both admission Workers.
pub const INTERFACE_TOKEN: &str = "ADMISSION_INTERFACE_TOKEN";

/// The bound of a cohort object's admission reply the Worker forwards: the
/// contract's response limit at R2's part maximum.
const REPLY_BYTES: usize = 64 * 1024 + 256 * crate::eval::plan::R2_MAX_PARTS as usize;
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

fn interface_token(env: &Env) -> Option<zeroize::Zeroizing<String>> {
    env.secret(INTERFACE_TOKEN).ok().map(|s| zeroize::Zeroizing::new(s.to_string()))
}

/// Whether a request is the upload Worker's: addressed to the internal
/// origin (no public route of this Worker carries that host) and carrying
/// the shared interface token.
fn from_upload_worker(req: &Request, env: &Env) -> bool {
    let host_ok = req.url().ok().and_then(|u| u.host_str().map(str::to_owned)).as_deref() == Some(iface::EVAL_INTERFACE_HOST);
    if !host_ok {
        return false;
    }
    let token = interface_token(env);
    let presented = req.headers().get(iface::TOKEN_HEADER).ok().flatten();
    iface::token_matches(token.as_deref().map(String::as_str), presented.as_deref())
}

/// The interface request body bound: a forwarded request's fields and at
/// most a session-creation body as hex (the shared
/// `slop_ninja_admission_wire::internal::FORWARD_BYTES`, the bound the
/// upload Worker sends within).
pub fn interface_body_bytes() -> usize {
    iface::FORWARD_BYTES
}

/// `/internal/admission/v1/<op>` (the upload Worker's service binding only):
/// the forwarded message goes to the cohort object's `/admission/<op>`, and
/// its JSON answer comes back as is (a `Reply`, or for `part-begin` a
/// target or a reply). The unsigned cohort read is not an operation here:
/// the upload Worker serves it from the cohort snapshot
/// ([`EvalCohort::write_cohort_snapshot`]). Anything not addressed and
/// authenticated as the upload Worker's answers 404, as does every request
/// when the eval role is off.
pub async fn interface_route(mut req: Request, env: Env) -> Result<Response> {
    if !eval_do::enabled(&env) || !from_upload_worker(&req, &env) || req.method() != Method::Post {
        return error("not_found", 404);
    }
    let path = req.path();
    let Some(op) = path.strip_prefix(iface::EVAL_PATH_PREFIX).filter(|op| iface::op::ALL.contains(op)) else {
        return error("not_found", 404);
    };
    let op = op.to_owned();
    // Re-encoded from its parse, so the object receives exactly the
    // interface's shape.
    let f: Forward = match super::cf_io::request_json(&mut req, interface_body_bytes()).await {
        Ok(f) => f,
        Err(e) => return error(e.code(), e.status()),
    };
    let body = serde_json::to_string(&f).map_err(|_| worker::Error::RustError("forward".into()))?;
    let mut r = cohort_fetch(&env, engine::INSTANCE_MAIN, Method::Post, &format!("/admission/{op}"), Some(body)).await?;
    relay(&mut r).await
}

/// A cohort object's answer, relayed within the reply bound.
async fn relay(r: &mut Response) -> Result<Response> {
    if r.status_code() != 200 {
        super::cf_io::discard(r);
        return error("cohort_unavailable", 503);
    }
    match super::cf_io::response_json::<Value>(r, REPLY_BYTES).await {
        Ok(v) => {
            let mut resp = Response::from_json(&v)?;
            resp.headers_mut().set("Cache-Control", "no-store")?;
            Ok(resp)
        }
        Err(_) => error("cohort_reply", 502),
    }
}

// ------------------------------------------------------- cohort object side

type AdmissionBody = Forward;

fn tx_or_unavailable<T>(r: std::result::Result<T, TxError<engine::EvalError>>) -> std::result::Result<T, Reply> {
    r.map_err(|_| {
        let mut e = admission::ApiError::new(slop_ninja_admission_wire::ErrorCode::Unavailable, "store");
        e.retry_after = Some(2);
        admission::reply_error(&e)
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
        let b: AdmissionBody = match super::cf_io::request_json(req, interface_body_bytes()).await {
            Ok(b) => b,
            Err(e) => return error(e.code(), e.status()),
        };
        let now = now_ms();
        // The route fields must be the signed request's own (the key-holding
        // side re-derives what the upload Worker parsed).
        let op = path.strip_prefix("/admission/").unwrap_or_default();
        if !admission::forward_matches(op, &b) {
            let reply = admission::forward_mismatch();
            return if op == iface::op::PART_BEGIN { json(&j!({"reply": reply})) } else { json(&reply) };
        }
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
                // The owner's session whose multipart upload is not yet
                // created gets it now: the request is authenticated and its
                // ownership checked first, so no other request creates one.
                let (id, c2, signed) = (b.session.clone(), chain.clone(), b.signed.clone());
                if let Ok(Some((key, attempt))) =
                    transaction(&storage, move |sql| Ok::<_, engine::EvalError>(admission::upload_missing_for(sql, &c2, &signed, &id)))
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
                    Ok::<_, engine::EvalError>(admission::part_record(sql, &chain, &b.signed, &b.session, &t, &b.etag))
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

    /// Writes the cohort snapshot (`internal::CohortSnapshot`) to the upload
    /// bucket: the cohort response as this object answers it now, and when.
    /// Called at every heartbeat (`/kick`) of the configured main authority,
    /// so the snapshot follows the cursor about once a block. A failed write
    /// is left to the next heartbeat; a snapshot over the bound the upload
    /// Worker reads within is not written.
    pub(super) async fn write_cohort_snapshot(&self) {
        let Some(chain) = chain(&self.env) else { return };
        let Ok(bucket) = self.env.bucket(ADMISSION_BUCKET) else { return };
        let paused = !self.fresh();
        let storage = self.state.storage();
        let Ok(cohort) = transaction(&storage, move |sql| admission::cohort_info(sql, &chain, paused)) else {
            return;
        };
        let snap = iface::CohortSnapshot {
            schema: iface::COHORT_SNAPSHOT_SCHEMA.into(),
            written_ms: now_ms(),
            cohort,
        };
        let Ok(bytes) = serde_json::to_vec(&snap) else { return };
        if bytes.len() > iface::cohort_snapshot_limit() {
            return;
        }
        let _ = bucket.put(iface::COHORT_SNAPSHOT_KEY, bytes).execute().await;
    }

    /// Performs a session operation's R2 control effects after its
    /// transaction committed, then records their results.
    async fn effects(&self, fx: Effects) {
        let Ok(bucket) = self.env.bucket(ADMISSION_BUCKET) else {
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
            let stored = transaction(&storage, move |sql| {
                admission::set_upload_id(sql, &session, attempt, &id)
            });
            // Another creation stored its upload first (or the record
            // failed): this one would be orphaned, so it is aborted.
            if !matches!(stored, Ok(true)) {
                let _ = u.abort().await;
            }
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
        let bucket = self.env.bucket(ADMISSION_BUCKET).map_err(|_| "upload bucket")?;
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
                // A named main authority installs a template only once every
                // earlier authority recorded it as its successor; a template
                // it would refuse supersedes nothing.
                if let Err(e) = calendar::check(&cfg, &bytes) {
                    let mut resp = Response::from_json(&j!({"error": e.code()}))?.with_status(422);
                    resp.headers_mut().set("Cache-Control", "no-store")?;
                    return Ok(resp);
                }
                if let Err(code) = self.succeed(&cfg).await {
                    return error(code, 409);
                }
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
                    // One row: the newest cohort whose plan has a fanout
                    // block, never every cohort's plan.
                    let rows = sql.exec(
                        "SELECT seq FROM cohorts WHERE json_extract(plan, '$.fanout') IS NOT NULL ORDER BY seq DESC LIMIT 1",
                        &[],
                    )?;
                    let last = match rows.first() {
                        Some(r) => engine::cohort(sql, crate::sql::int(r, 0)?)?.plan.fanout.map(|f| f.cost),
                        None => None,
                    };
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
                    /// The planning invocation's remaining time: how long a
                    /// planned round's claim lasts unless its ingest ends it.
                    lease_ms: u64,
                    /// Its time left once this call returns (at the call's
                    /// allowance): the round keeps the reads that fit.
                    #[serde(default)]
                    fit_ms: Option<u64>,
                    /// The Worker's observations of its previous calls.
                    #[serde(default)]
                    prior: commitments::Prior,
                }
                let b: B = match super::cf_io::request_json(req, 1024).await {
                    Ok(b) => b,
                    Err(e) => return error(e.code(), e.status()),
                };
                let r = transaction(&storage, move |sql| {
                    commitments::note_prior(sql, &b.prior)?;
                    let o = engine::observed(sql)?;
                    let needs = commitments::Needs {
                        poll: admission::window(sql)?.is_some_and(|w| w.admitting()),
                        snapshot: admission::snapshot_needs(sql)?,
                    };
                    // The stored metadata is parsed only when a round could
                    // be planned (nothing is due between windows).
                    let rt = if commitments::wants_round(sql, &needs, now)? { runtime(sql)? } else { None };
                    commitments::plan_claimed(sql, rt.as_ref(), netuid, o.confirmed, &needs, b.per_minute, now, b.lease_ms, b.fit_ms)
                });
                match r {
                    Ok(round) => json(&round),
                    Err(_) => error("store_unavailable", 503),
                }
            }
            "/commitments/starved" => match transaction(&storage, commitments::note_starved) {
                Ok(()) => json(&serde_json::json!({"ok": true})),
                Err(_) => error("store_unavailable", 503),
            },
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
                    // Not parsed for a claim release or the runtime round.
                    let rt = if commitments::ingest_needs_runtime(&b.results) { runtime(sql)? } else { None };
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

    /// One framing attempt: the upload Worker streams the object and frames
    /// it (the one decode of an uploaded object, outside this key-holding
    /// Worker) and answers with its `FrameOutcome`. A failed call is
    /// recorded by its kind ([`FrameFailure`]): only the upload Worker's
    /// explicit object-read answer is uncounted, and a link that is not
    /// configured faults the cohort at once.
    async fn frame(&self, session: &str) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let id = session.to_owned();
        let now = now_ms();
        let Some((key, len, version)) =
            transaction(&storage, move |sql| admission::frame_begin(sql, &id, now)).map_err(|_| "frame target")?
        else {
            return Ok(());
        };
        let framed = match frame_call(&self.env, &key, len, version.id()).await {
            Ok(o) => o,
            Err((kind, e)) => {
                let id = session.to_owned();
                let now = now_ms();
                let _ = transaction(&storage, move |sql| admission::frame_failed(sql, &id, kind, now));
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
        let uploads = self.env.bucket(ADMISSION_BUCKET).map_err(|_| "upload bucket")?;
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
            copy_range(&uploads, &key, sl.offset, sl.len, &bucket, &blob).await?;
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
        // Each run is recorded as soon as it finishes, with its instance time
        // at the cohort's Container rates (one charge key per hotkey,
        // commitment and try), so a later run cut off by the platform never
        // drops an earlier run's outcome or charge. The settle in the tracking
        // ledger (never refused) is best effort here: a charge it did not
        // confirm stays recorded and the step settles it later
        // (`Work::AdmissionSettle`).
        let storage = &storage;
        let results = futures_util::future::join_all(runs.into_iter().map(|run| {
            let env = env.clone();
            async move {
                let name = run.lane_name(crate::config::NETWORK);
                // The lane's admission route: a fresh instance, the cohort's
                // label, spec digest and lane limits, and the definitive flag.
                let body = admission::admit_body(&run);
                let t0 = now_ms();
                let outcome = async {
                    let Ok(stub) = lane_stub(&self.env, "score", &name) else {
                        return LaneOutcome::Platform("lane_binding".into());
                    };
                    let Ok(req) = eval_do::internal_request(Method::Post, "/admit", Some(body.to_string())) else {
                        return LaneOutcome::Platform("lane_request".into());
                    };
                    match stub.fetch_with_request(req).await {
                        Ok(mut r) => {
                            let status = r.status_code();
                            let v: Value = super::cf_io::response_json(&mut r, 64 * 1024)
                                .await
                                .unwrap_or(Value::Null);
                            admission::classify_admit(Some(status), &v)
                        }
                        Err(_) => admission::classify_admit(None, &Value::Null),
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
                let wall_ms = now_ms().saturating_sub(t0);
                let now = now_ms();
                let charge = transaction(storage, move |sql| {
                    let bound = admission::failures_bound(sql, &run.label)?;
                    admission::record_run(sql, &env, &run, &outcome, bound, now)?;
                    admission::record_charge(sql, &run, wall_ms, now)
                })
                .map_err(|_| "record run")?;
                if let Some((key, micro)) = charge
                    && eval_do::ledger_call(
                        &self.env,
                        "/settle",
                        &j!({"key": key, "microusd": micro, "note": "admission run"}),
                    )
                    .await
                    .is_ok()
                {
                    let _ = transaction(storage, move |sql| admission::charge_settled(sql, &key));
                }
                Ok::<(), &'static str>(())
            }
        }))
        .await;
        results.into_iter().collect()
    }

    async fn cleanup(&self) -> std::result::Result<(), &'static str> {
        let storage = self.state.storage();
        let now = now_ms();
        let due = transaction(&storage, move |sql| admission::cleanup_due(sql, eval_do::PARALLEL as i64, now))
            .map_err(|_| "cleanup due")?;
        // Uploaded objects and their multipart uploads are in the upload
        // bucket; the blobs and manifest a session's copies created are in
        // the eval bucket.
        let uploads = self.env.bucket(ADMISSION_BUCKET).map_err(|_| "upload bucket")?;
        let bucket = self.env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket")?;
        // A delete the bucket does not confirm backs its row off and the
        // others still run (`admission::cleanup_failed`,
        // `object_delete_failed`): housekeeping never fails the step.
        for (id, key, upload_id) in due {
            if let Some(u) = upload_id
                && let Ok(m) = uploads.resume_multipart_upload(key.clone(), u)
            {
                let _ = m.abort().await;
            }
            let mut ok = uploads.delete(key).await.is_ok();
            if ok {
                // The blobs and manifest only this session's copies created.
                let sid = id.clone();
                let extra = transaction(&storage, move |sql| admission::cleanup_keys(sql, &sid))
                    .map_err(|_| "cleanup keys")?;
                for k in extra {
                    if bucket.delete(k).await.is_err() {
                        ok = false;
                        break;
                    }
                }
            }
            let now = now_ms();
            if ok {
                transaction(&storage, move |sql| admission::mark_cleaned(sql, &id)).map_err(|_| "cleanup commit")?;
            } else {
                transaction(&storage, move |sql| admission::cleanup_failed(sql, &id, now))
                    .map_err(|_| "cleanup commit")?;
            }
        }
        // Aborted attempts' objects: the abort's own delete is best effort;
        // each stays recorded until the bucket confirms it here.
        let pending =
            transaction(&storage, move |sql| admission::object_deletes_due(sql, eval_do::PARALLEL as i64, now))
                .map_err(|_| "object deletes due")?;
        for (key, upload_id) in pending {
            if let Some(u) = upload_id
                && let Ok(m) = uploads.resume_multipart_upload(key.clone(), u)
            {
                let _ = m.abort().await;
            }
            let ok = uploads.delete(key.clone()).await.is_ok();
            let now = now_ms();
            transaction(&storage, move |sql| {
                if ok { admission::object_deleted(sql, &key) } else { admission::object_delete_failed(sql, &key, now) }
            })
            .map_err(|_| "object delete commit")?;
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
        // The calendar of a named authority opens nothing until every
        // earlier authority recorded it as its successor (`succeed`, done
        // once at the template's install or the first plan).
        if let Err(code) = self.succeed(cfg).await {
            let _ = transaction(&storage, move |sql| {
                calendar::note_error(sql, code)?;
                calendar::set_paused(sql, true).map(|_| ())
            });
            return Err("template predecessors");
        }
        let key = format!("{}:{label}", engine::INSTANCE_MAIN);
        // `created` is false when the key was already held (a retried open):
        // then a refused queue leaves it alone, as the operator `/plan` does.
        let created = match eval_do::ledger_call(
            &self.env,
            "/reserve",
            &j!({"key": key, "microusd": adm.reserve_microusd}),
        )
        .await
        {
            Ok(v) => v.get("created").and_then(Value::as_bool) == Some(true),
            Err(_) => false,
        };
        let c2 = cfg.clone();
        let now = now_ms();
        match transaction(&storage, move |sql| engine::queue(sql, &c2, &plan, now)) {
            Ok(_) => Ok(()),
            Err(TxError::Operation(e)) => {
                // A refused queue never leaves an orphan reservation that
                // every later cycle's total would count.
                if created {
                    let _ = eval_do::ledger_call(&self.env, "/release", &j!({"key": key})).await;
                }
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

/// Copies `len` bytes at `offset` of `key` in `source` (the upload bucket)
/// to `dest` in `bucket` (the eval bucket) as a JS stream (no byte enters
/// Rust).
async fn copy_range(
    source: &worker::Bucket,
    key: &str,
    offset: u64,
    len: u64,
    bucket: &worker::Bucket,
    dest: &str,
) -> std::result::Result<(), &'static str> {
    let src: &JsValue = source.as_ref();
    let b: &JsValue = bucket.as_ref();
    let obj = match super::cf_io::settle(adm_get(src, key, Some(offset as f64), Some(len as f64)), None).await {
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

/// The upload Worker's framing route, through this Worker's service
/// binding, within the alarm's own wall limit; the answer is read within
/// the largest outcome a spec-admitted object can produce. A failure comes
/// with its kind ([`FrameFailure`]).
async fn frame_call(env: &Env, key: &str, len: u64, spec: &str) -> std::result::Result<FrameOutcome, (FrameFailure, &'static str)> {
    use FrameFailure::{Counted, Misconfigured, ObjectRead};
    let token = interface_token(env).ok_or((Misconfigured, "interface token"))?;
    let body = serde_json::to_string(&iface::FrameRequest { key: key.to_owned(), len, spec: spec.to_owned() })
        .map_err(|_| (Counted, "frame request"))?;
    let url = format!("{}{}", iface::UPLOAD_INTERFACE_ORIGIN, iface::UPLOAD_FRAME_PATH);
    let req = super::cf_io::internal_request(&url, Method::Post, Some(&body), Some(slop_ninja_arena::limits::ALARM_MS))
        .map_err(|_| (Counted, "frame request"))?;
    req.headers().set(iface::TOKEN_HEADER, token.as_str()).map_err(|_| (Counted, "frame request"))?;
    let service = env.service(UPLOAD_SERVICE).map_err(|_| (Misconfigured, "upload service"))?;
    let mut r = service.fetch_request(req).await.map_err(|_| (Counted, "frame call"))?;
    let status = r.status_code();
    if status != 200 {
        // The upload Worker marks its platform's failures (the object read,
        // the request body unread, an arena refusal): retried uncounted.
        let marked = r.headers().get(iface::FRAME_FAILURE_HEADER).ok().flatten();
        let platform = matches!(marked.as_deref(), Some(iface::FRAME_OBJECT_READ | iface::FRAME_PLATFORM));
        super::cf_io::discard(&mut r);
        return Err(match status {
            503 if platform => (ObjectRead, "frame platform failure"),
            400 | 404 => (Misconfigured, "frame link"),
            _ => (Counted, "frame unavailable"),
        });
    }
    super::cf_io::response_json::<FrameOutcome>(&mut r, slop_ninja_artifact_check::admission::frame_outcome_bound())
        .await
        .map_err(|_| (Counted, "frame reply"))
}

/// The eval-to-upload link probe (`GET /eval/admission/link`, status
/// token): one framing request for the probe key
/// (`internal::FRAME_PROBE_KEY`), which the upload Worker answers without
/// reading anything once the binding, the internal host and the shared
/// token all work. `{"frame": "ok"}`, else the failure.
pub async fn link_probe(env: &Env) -> Value {
    let probe = async {
        let token = interface_token(env).ok_or("interface token not configured")?;
        let body = serde_json::to_string(&iface::FrameRequest {
            key: iface::FRAME_PROBE_KEY.to_owned(),
            len: 0,
            spec: String::new(),
        })
        .map_err(|_| "frame request")?;
        let url = format!("{}{}", iface::UPLOAD_INTERFACE_ORIGIN, iface::UPLOAD_FRAME_PATH);
        let req = super::cf_io::internal_request(&url, Method::Post, Some(&body), None).map_err(|_| "frame request")?;
        req.headers().set(iface::TOKEN_HEADER, token.as_str()).map_err(|_| "frame request")?;
        let service = env.service(UPLOAD_SERVICE).map_err(|_| "upload service binding missing")?;
        let mut r = service.fetch_request(req).await.map_err(|_| "frame call failed")?;
        if r.status_code() != 200 {
            super::cf_io::discard(&mut r);
            return Err(if r.status_code() == 404 { "upload Worker refused the host or token (404)" } else { "upload Worker answered an error" });
        }
        let v: Value = super::cf_io::response_json(&mut r, 1024).await.map_err(|_| "frame reply")?;
        if v.get("probe").and_then(Value::as_str) == Some("ok") { Ok(()) } else { Err("frame reply") }
    };
    match probe.await {
        Ok(()) => j!({"frame": "ok"}),
        Err(e) => j!({"frame": e}),
    }
}

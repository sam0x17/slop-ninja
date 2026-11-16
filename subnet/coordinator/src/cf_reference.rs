//! Reference staging (`slop_ninja_reference_staging_v1`) on Workers
//! (wasm32 only): the eval Worker's two
//! operator routes for the operator's reference packages
//! ([`crate::eval::reference`] holds the decisions, natively tested).
//!
//! - `POST /eval/reference/v2/urls`: the internal signer mints one
//!   presigned `PUT` per reference file (entry kind `Reference`), through
//!   the same service binding and token as admission's mints. The operator's
//!   client PUTs each file straight to R2; no file byte reaches this Worker.
//! - `POST /eval/reference/v2/commit?spec=<spec id>`: heads every file in
//!   the admission bucket, pins its entity tag and writes the pinned files
//!   manifest to the eval bucket with R2 checking its SHA-256.
//!
//! Both need the operator token (`COORDINATOR_OPERATOR_TOKEN`) and the eval
//! role, and run in their own arena route (`arena_routes::R_REFERENCE`).
//! Nothing here logs a URL, a body or a token.

use super::admission_do;
use super::eval_do::{self, EVAL_BUCKET};
use super::{bearer_ok, cf_io, error, unauthorized};
use crate::access;
use crate::eval::admission::Head;
use crate::eval::reference::{self, RefError};
use js_sys::Uint8Array;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use worker::{Env, Method, Request, Response, Result};

#[wasm_bindgen(inline_js = "
export function ref_put_verified(bucket, key, bytes, sha256hex) {
  return bucket.put(key, bytes, { sha256: sha256hex }).then((o) => (o ? o.etag : null));
}
export function ref_copy(src, srcKey, etag, dst, dstKey, len, sha256hex) {
  return src.get(srcKey, etag ? { onlyIf: { etagMatches: etag } } : {}).then((o) => {
    if (o === null) return 'absent';
    if (!o.body) return 'changed';
    const f = new FixedLengthStream(len);
    const pipe = o.body.pipeTo(f.writable).then(() => true, () => false);
    return Promise.all([dst.put(dstKey, f.readable, { sha256: sha256hex }), pipe]).then(
      ([p, piped]) => (p && piped ? 'copied' : 'put_failed'),
      () => 'put_failed');
  }, () => 'get_failed');
}
")]
extern "C" {
    /// A binding `put` R2 verifies against `sha256hex` (refused on a
    /// mismatch); resolves to the stored entity tag.
    fn ref_put_verified(bucket: &JsValue, key: &str, bytes: &Uint8Array, sha256hex: &str) -> js_sys::Promise;
    /// Copies `srcKey` (read pinned to `etag`, when not empty) to `dstKey`, streamed at its
    /// exact length with R2 verifying `sha256hex` on arrival: `copied`,
    /// `absent`, `changed` (another entity tag), `get_failed` or `put_failed`.
    fn ref_copy(src: &JsValue, src_key: &str, etag: &str, dst: &JsValue, dst_key: &str, len: f64, sha256hex: &str) -> js_sys::Promise;
}

fn answer(e: &RefError) -> Result<Response> {
    let mut r = Response::from_json(&e.body())?.with_status(e.status());
    r.headers_mut().set("Cache-Control", "no-store")?;
    Ok(r)
}

fn ok<T: serde::Serialize>(v: &T) -> Result<Response> {
    let mut r = Response::from_json(v)?;
    r.headers_mut().set("Cache-Control", "no-store")?;
    Ok(r)
}

/// The routes under `/eval/reference/`.
pub async fn route(mut req: Request, env: Env) -> Result<Response> {
    if !eval_do::enabled(&env) {
        return error("not_found", 404);
    }
    match bearer_ok(&req, &env, access::OPERATOR_TOKEN) {
        Err(()) => return error("token_not_configured", 503),
        Ok(false) => return unauthorized(),
        Ok(true) => {}
    }
    let path = req.path();
    match (req.method(), path.as_str()) {
        (Method::Post, reference::URLS_PATH) => urls(&mut req, &env).await,
        (Method::Post, reference::COMMIT_PATH) => commit(&mut req, &env).await,
        (_, reference::URLS_PATH | reference::COMMIT_PATH) => error("method_not_allowed", 405),
        _ => error("not_found", 404),
    }
}

async fn urls(req: &mut Request, env: &Env) -> Result<Response> {
    let body = match cf_io::request_bytes(req, reference::URLS_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => return error(e.code(), e.status()),
    };
    let entries = match reference::mint_entries(&body) {
        Ok(e) => e,
        Err(e) => return answer(&e),
    };
    match admission_do::mint_urls(env, &entries).await {
        Ok(minted) => match reference::urls_reply(&entries, &minted) {
            Ok(v) => ok(&v),
            Err(e) => answer(&e),
        },
        Err(_) => answer(&RefError::Platform("signer_unavailable")),
    }
}

async fn commit(req: &mut Request, env: &Env) -> Result<Response> {
    let spec = req.url()?.query_pairs().find(|(k, _)| k == "spec").map(|(_, v)| v.into_owned());
    let Some(spec) = spec else {
        return answer(&RefError::Request("spec"));
    };
    let body = match cf_io::request_bytes(req, reference::COMMIT_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => return error(e.code(), e.status()),
    };
    let checked = match reference::commit_request(&body, &spec) {
        Ok(c) => c,
        Err(e) => return answer(&e),
    };
    drop(body);
    let Ok(b) = admission_do::bucket(env) else {
        return answer(&RefError::Platform("admission bucket"));
    };
    let mut heads: Vec<Head> = Vec::with_capacity(checked.keys.len());
    for group in checked.keys.chunks(admission_do::HEADS_IN_FLIGHT) {
        heads.extend(futures_util::future::join_all(group.iter().map(|k| admission_do::head(&b, k))).await);
    }
    let pinned = match reference::pin(checked, &heads) {
        Ok(p) => p,
        Err(e) => return answer(&e),
    };
    let Ok(eval) = env.bucket(EVAL_BUCKET) else {
        return answer(&RefError::Platform("eval bucket"));
    };
    let key = crate::eval::admission::manifest_key(&pinned.package);
    let put = ref_put_verified(eval.as_ref(), &key, &Uint8Array::from(pinned.bytes.as_slice()), &pinned.package);
    match cf_io::settle(put, None).await {
        cf_io::Settled::Value(v) if v.as_string().is_some() => ok(&reference::commit_reply(&pinned)),
        _ => answer(&RefError::Platform("manifest put")),
    }
}

// ------------------------------------------------------------ reference pins

/// The bucket a files manifest names (`admission` or `eval`).
fn named_bucket(env: &Env, name: &str) -> std::result::Result<worker::Bucket, &'static str> {
    match name {
        "admission" => admission_do::bucket(env),
        "eval" => env.bucket(EVAL_BUCKET).map_err(|_| "eval bucket"),
        _ => Err("bucket"),
    }
}

impl eval_do::EvalCohort {
    /// One reference pin (`eval::pins`): reads the source's files manifest,
    /// copies each file not yet at its reference key (R2 verifying its
    /// SHA-256), heads the copies, pins their entity tags and writes the
    /// pinned manifest; then records the pinned record. A failure is
    /// recorded against the pin (retried after a tick, or held for the
    /// operator's recheck); the step itself never fails.
    pub(super) async fn reference_pin(&self, content: &str) -> std::result::Result<(), &'static str> {
        use crate::eval::pins;
        let storage = self.state.storage();
        let now = super::now_ms();
        let job = super::transaction(&storage, move |sql| pins::due(sql, now)).map_err(|_| "pin due")?;
        let Some(job) = job.filter(|j| j.content == content) else { return Ok(()) };
        let outcome = self.pin_once(&job).await;
        let (c, now) = (job.content.clone(), super::now_ms());
        super::transaction(&storage, move |sql| match outcome {
            Ok((record, copied)) => pins::pin_done(sql, &c, &record, copied, now),
            Err(f) => pins::pin_failed(sql, &c, &f, now),
        })
        .map_err(|_| "pin commit")?;
        Ok(())
    }

    async fn pin_once(
        &self,
        job: &crate::eval::pins::PinJob,
    ) -> std::result::Result<(crate::eval::plan::ArtifactJson, u64), crate::eval::pins::PinFailure> {
        use crate::eval::pins::{self, PinFailure};
        let bytes = match eval_do::get_bounded(&self.env, EVAL_BUCKET, &job.source.manifest_key, crate::eval::decide::SPAN_FILES_MANIFEST_BYTES).await {
            Ok(Some(b)) => b,
            Ok(None) => return Err(PinFailure::Permanent("source_manifest_absent")),
            Err(cf_io::BodyError::TooLarge) => return Err(PinFailure::Permanent("source_manifest_invalid")),
            Err(_) => return Err(PinFailure::Transient("source_manifest_read")),
        };
        let plan = pins::plan_copy(&bytes)?;
        drop(bytes);
        if plan.as_is {
            return Ok((job.source.clone(), 0));
        }
        let dst = admission_do::bucket(&self.env).map_err(PinFailure::Transient)?;
        let mut copied = 0u64;
        for item in &plan.items {
            match admission_do::head(&dst, &item.dst_key).await {
                Head::Present(st) => {
                    if st.size != item.bytes || st.sha256.map(hex::encode).as_deref() != Some(item.sha256.as_str()) {
                        return Err(PinFailure::Permanent("reference_mismatch"));
                    }
                    continue;
                }
                Head::Error => return Err(PinFailure::Transient("reference_head")),
                Head::Absent => {}
            }
            let src = named_bucket(&self.env, &item.src_bucket).map_err(PinFailure::Transient)?;
            let p = ref_copy(src.as_ref(), &item.src_key, item.src_etag.as_deref().unwrap_or_default(), dst.as_ref(), &item.dst_key, item.bytes as f64, &item.sha256);
            match cf_io::settle(p, None).await {
                cf_io::Settled::Value(v) => match v.as_string().as_deref() {
                    Some("copied") => copied += 1,
                    Some("absent") => return Err(PinFailure::Permanent("source_absent")),
                    Some("changed") => return Err(PinFailure::Permanent("source_changed")),
                    _ => return Err(PinFailure::Transient("copy_failed")),
                },
                _ => return Err(PinFailure::Transient("copy_failed")),
            }
        }
        let mut heads: Vec<Head> = Vec::with_capacity(plan.items.len());
        for group in plan.items.chunks(admission_do::HEADS_IN_FLIGHT) {
            heads.extend(futures_util::future::join_all(group.iter().map(|c| admission_do::head(&dst, &c.dst_key))).await);
        }
        let pinned = pins::finish(plan, &heads)?;
        let eval = self.env.bucket(EVAL_BUCKET).map_err(|_| PinFailure::Transient("eval bucket"))?;
        let key = crate::eval::admission::manifest_key(&pinned.package);
        let put = ref_put_verified(eval.as_ref(), &key, &Uint8Array::from(pinned.bytes.as_slice()), &pinned.package);
        match cf_io::settle(put, None).await {
            cf_io::Settled::Value(v) if v.as_string().is_some() => Ok((pins::pinned_record(&job.source, &pinned.package), copied)),
            _ => Err(PinFailure::Transient("manifest_put")),
        }
    }

    /// One presence check (`eval::pins::presence_due`): heads every file of
    /// each target package against the entity tag its files manifest pins,
    /// and records the outcome.
    pub(super) async fn reference_presence(&self, cfg: &crate::eval::engine::EvalConfig) -> std::result::Result<(), &'static str> {
        use crate::eval::pins::{self, Checked, FileCheck};
        use slop_ninja_artifact_check::files_manifest::FilesManifest;
        let storage = self.state.storage();
        let (first, now) = (cfg.authority.first_cohort(), super::now_ms());
        let job = super::transaction(&storage, move |sql| pins::presence_due(sql, first, now)).map_err(|_| "presence due")?;
        let Some(job) = job else { return Ok(()) };
        let mut checked: Vec<Checked> = Vec::new();
        for t in &job.targets {
            let at = |file: &str, key: &str, outcome: FileCheck| Checked {
                package: t.package.clone(),
                file: file.to_owned(),
                key: key.to_owned(),
                outcome,
            };
            let m = match eval_do::get_bounded(&self.env, EVAL_BUCKET, &t.manifest_key, crate::eval::decide::SPAN_FILES_MANIFEST_BYTES).await {
                Ok(Some(b)) => serde_json::from_slice::<FilesManifest>(&b).ok(),
                Ok(None) => {
                    checked.push(at("files_manifest", &t.manifest_key, FileCheck::Missing("manifest_absent")));
                    continue;
                }
                Err(_) => {
                    checked.push(at("files_manifest", &t.manifest_key, FileCheck::Error));
                    continue;
                }
            };
            let Some(m) = m else {
                checked.push(at("files_manifest", &t.manifest_key, FileCheck::Missing("manifest_invalid")));
                continue;
            };
            for group in m.files.chunks(admission_do::HEADS_IN_FLIGHT) {
                let heads = futures_util::future::join_all(group.iter().map(|f| async {
                    match named_bucket(&self.env, &f.bucket) {
                        Ok(b) => admission_do::head(&b, &f.key).await,
                        Err(_) => Head::Error,
                    }
                }))
                .await;
                for (f, h) in group.iter().zip(&heads) {
                    checked.push(at(&f.name, &f.key, pins::check_file(f, h)));
                }
            }
        }
        let now = super::now_ms();
        super::transaction(&storage, move |sql| pins::presence_done(sql, &job, &checked, now).map(|_| ()))
            .map_err(|_| "presence commit")
    }
}

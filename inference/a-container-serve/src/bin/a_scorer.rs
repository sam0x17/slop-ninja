//! a-scorer: batch scoring of one admitted *A* artifact per instance, loaded by digest.
//!
//! The image carries no model. The coordinator streams an artifact's content-addressed
//! parts into the container (`PUT /v1/blobs/<sha256>`), then posts the artifact files
//! manifest (`POST /v1/load?package=<sha256>[&cohort=<id>][&spec=<spec sha256>]`). The
//! load admits by the artifact spec it names by digest, the cohort's (spec v2 or v3),
//! and by spec v2 when it names none (`engine::UNNAMED_LOAD_SPEC`). The server checks
//! that the manifest bytes hash to the package digest and that it lists that spec's
//! file table within the spec's bounds (`limits`), assembles each file from its parts
//! with every part and file digest checked, deletes the parts, runs the spec's whole
//! admission check (graph pre-parse included) before ONNX Runtime opens anything,
//! repeats the release contract guards and loads the f32 graph (`engine`).
//! `POST /v1/score` then scores a batch of documents for that package and returns,
//! per document, digests, counts and the prediction; with `"spans": true` and a token
//! head, also sentences and spans.
//!
//! **Single use.** The first accepted load binds the process to its package, cohort
//! (absent cohort included) and spec for its whole life, whatever the load's outcome.
//! A load of anything else, a part upload after binding, or a score request for
//! another package or cohort is refused with 409 `instance_bound`: no process holds
//! a second artifact or serves a second cohort it was told about. At start the
//! process removes the work directory's `blobs/` and `files/`, so a process
//! restarted in the same container begins with no artifact on disk. The cohort
//! half binds only when the caller names the cohort (`cohort=` on load, `cohort`
//! on score); a load without one binds the absent cohort, which any later request
//! without one matches. Starting a fresh instance for each artifact and cohort is
//! the coordinator's part.
//!
//! The container runs with Internet access disabled and makes no outbound calls. It
//! logs no text, document ids, token ids or digests: only start, load and failure
//! events. `/healthz` reports the build identity (the SHA-256 of this binary and of
//! the ONNX Runtime library, computed at startup, and the sessions and threads in
//! use), so the coordinator can refuse an instance whose build differs from the
//! pinned digests: Worker code goes live before a Container rollout finishes. A panic
//! anywhere aborts the process, so a crash is always a container death to the
//! coordinator's crash isolation. Environment: A_HOST (0.0.0.0), A_PORT (8080),
//! A_SESSIONS (1-8, default 4), A_THREADS (1-16, default 1), A_WORK_DIR (/tmp/a-scorer),
//! A_ORT_LIB (/opt/ort/libonnxruntime.so.1.31.1).
use a_container_serve::artifact::{self, ArtifactFiles, MAX_MANIFEST_BYTES, MAX_PART_BYTES};
use a_container_serve::engine::Model;
use a_container_serve::instance::{Binding, Load, Put as PartPut, load_query};
use a_container_serve::limits::{self, MAX_DOCS, MAX_INFLIGHT, MAX_SCORE_BODY};
use a_container_serve::profile::ServedProfile;
use anyhow::{Context, Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use slop_ninja_artifact_check::spec::Version;
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Instant;

struct Config {
    host: String,
    port: u16,
    sessions: usize,
    threads: usize,
    work: PathBuf,
    lib: String,
}

enum Slot {
    Empty,
    Loading { bound: Binding, since: Instant },
    Ready { bound: Binding, model: Arc<Model>, load_ms: f64 },
    Failed { bound: Binding, error: String },
}

impl Slot {
    fn bound(&self) -> Option<&Binding> {
        match self {
            Slot::Empty => None,
            Slot::Loading { bound, .. } | Slot::Ready { bound, .. } | Slot::Failed { bound, .. } => Some(bound),
        }
    }
}

struct State {
    slot: Mutex<Slot>,
    /// Part digests of the bound package's manifest. They count as present after
    /// the parts are deleted, so a coordinator re-entering a bound instance with
    /// the same package skips the upload instead of meeting `instance_bound`.
    bound_parts: Mutex<std::collections::BTreeSet<String>>,
    start: Instant,
    scored: AtomicUsize,
    build: Value,
}

fn file_sha256(path: &std::path::Path) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(hex::encode(h.finalize()))
}

fn respond(req: tiny_http::Request, code: u16, v: &Value) {
    let body = serde_json::to_vec(v).unwrap_or_default();
    let h = tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..]).unwrap();
    let _ = req.respond(tiny_http::Response::from_data(body).with_status_code(code).with_header(h));
}

fn read_trim(p: &str) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// Process CPU time (all threads) in ms, from /proc/self/stat (USER_HZ = 100), and the
/// container cgroup's CPU usage in ms when visible.
fn cpu_ms() -> Value {
    let process = fs::read_to_string("/proc/self/stat").ok().and_then(|s| {
        let rest = s.rsplit_once(')')?.1;
        let f: Vec<&str> = rest.split_whitespace().collect();
        let ut: u64 = f.get(11)?.parse().ok()?;
        let st: u64 = f.get(12)?.parse().ok()?;
        Some((ut + st) * 10)
    });
    let cgroup = read_trim("/sys/fs/cgroup/cpu.stat").and_then(|s| {
        s.lines().find_map(|l| l.strip_prefix("usage_usec ")?.parse::<u64>().ok()).map(|u| u / 1000)
    });
    json!({"process": process, "cgroup": cgroup})
}

fn slot_json(state: &State) -> Value {
    let slot = state.slot.lock().unwrap();
    let mut v = match &*slot {
        Slot::Empty => json!({"state": "empty"}),
        Slot::Loading { bound, since } => json!({"state": "loading", "package": bound.package,
            "cohort": bound.cohort, "spec": bound.spec.id(), "loading_ms": since.elapsed().as_secs_f64() * 1e3}),
        Slot::Ready { bound, model, load_ms } => json!({"state": "ready", "package": bound.package,
            "cohort": bound.cohort, "spec": bound.spec.id(), "load_ms": load_ms, "load": model.load,
            "spans": model.emits_spans()}),
        Slot::Failed { bound, error } => json!({"state": "failed", "package": bound.package,
            "cohort": bound.cohort, "spec": bound.spec.id(), "error": error}),
    };
    v["single_use"] = json!(true);
    v["uptime_ms"] = json!(state.start.elapsed().as_secs_f64() * 1e3);
    v["scored"] = json!(state.scored.load(SeqCst));
    v["cpu_ms"] = cpu_ms();
    v["build"] = state.build.clone();
    v
}

fn blob_path(cfg: &Config, sha: &str) -> PathBuf {
    cfg.work.join("blobs").join(sha)
}

/// Stream a request body to a part file, checking its digest and size.
fn put_blob(cfg: &Config, sha: &str, reader: &mut dyn Read) -> Result<u64> {
    let dir = cfg.work.join("blobs");
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{sha}.{}.part", std::process::id()));
    let mut w = fs::File::create(&tmp)?;
    let mut h = Sha256::new();
    let mut total = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_PART_BYTES {
            let _ = fs::remove_file(&tmp);
            return Err(anyhow!("part larger than {MAX_PART_BYTES} bytes"));
        }
        h.update(&buf[..n]);
        w.write_all(&buf[..n])?;
    }
    w.sync_all()?;
    drop(w);
    if hex::encode(h.finalize()) != sha {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow!("part digest differs"));
    }
    fs::rename(&tmp, blob_path(cfg, sha))?;
    Ok(total)
}

/// Assemble and load `manifest` for `package` (runs on a loader thread). The parts
/// are deleted once every file is assembled; admission by `spec` runs inside
/// `Model::load` before ONNX Runtime opens the graph.
fn load(cfg: &Config, package: &str, spec: Version, manifest: &ArtifactFiles) -> Result<Model> {
    let dir = cfg.work.join("files").join(package);
    fs::create_dir_all(&dir)?;
    let blobs = cfg.work.join("blobs");
    let assembled = manifest.files.iter().try_for_each(|f| artifact::assemble(f, &blobs, &dir.join(&f.name)));
    let _ = fs::remove_dir_all(&blobs);
    assembled?;
    Model::load(&dir, spec, &manifest.graph, cfg.sessions, cfg.threads, &cfg.lib)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScoreRequest {
    package: String,
    /// The cohort the instance was loaded for (absent when the load named none).
    #[serde(default)]
    cohort: Option<String>,
    /// Also return sentences and spans when the artifact has a token head.
    #[serde(default)]
    spans: bool,
    docs: Vec<Doc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    id: String,
    text: String,
}

fn score(cfg: &Config, state: &State, body: &[u8]) -> Result<(u16, Value)> {
    let t0 = Instant::now();
    let cpu0 = cpu_ms();
    let r: ScoreRequest = serde_json::from_slice(body).context("score request JSON")?;
    ensure!(!r.docs.is_empty() && r.docs.len() <= MAX_DOCS, "1 to {MAX_DOCS} documents per batch");
    ensure!(r.docs.iter().all(|d| !d.id.is_empty() && d.id.len() <= 128), "document id length");
    let serves = |b: &Binding| b.serves(&r.package, r.cohort.as_deref());
    let model = match &*state.slot.lock().unwrap() {
        Slot::Ready { bound, model, .. } if serves(bound) => model.clone(),
        slot if slot.bound().is_some_and(|b| !serves(b)) => {
            return Ok((409, json!({"error": "instance_bound"})));
        }
        _ => return Ok((409, json!({"error": "package_not_ready", "slot": Value::Null}))),
    };
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Value>> = r.docs.iter().map(|_| Mutex::new(Value::Null)).collect();
    std::thread::scope(|s| {
        for _ in 0..cfg.sessions.min(r.docs.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, SeqCst);
                    let Some(d) = r.docs.get(i) else { break };
                    *results[i].lock().unwrap() = model.score_with(&d.id, &d.text, r.spans);
                }
            });
        }
    });
    state.scored.fetch_add(r.docs.len(), SeqCst);
    let results: Vec<Value> = results.into_iter().map(|m| m.into_inner().unwrap()).collect();
    Ok((200, json!({"package": r.package, "results": results,
        "timing_ms": {"total": t0.elapsed().as_secs_f64() * 1e3},
        "cpu_ms_before": cpu0, "cpu_ms_after": cpu_ms()})))
}

fn main() -> Result<()> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let cfg = Arc::new(Config {
        host: env("A_HOST").unwrap_or_else(|| "0.0.0.0".into()),
        port: env("A_PORT").map(|v| v.parse()).transpose()?.unwrap_or(8080),
        sessions: env("A_SESSIONS").map(|v| v.parse()).transpose()?.unwrap_or(4),
        threads: env("A_THREADS").map(|v| v.parse()).transpose()?.unwrap_or(1),
        work: env("A_WORK_DIR").map(PathBuf::from).unwrap_or_else(|| "/tmp/a-scorer".into()),
        lib: env("A_ORT_LIB").unwrap_or_else(|| "/opt/ort/libonnxruntime.so.1.31.1".into()),
    });
    ensure!((1..=8).contains(&cfg.sessions) && (1..=16).contains(&cfg.threads), "A_SESSIONS/A_THREADS range");
    // A panic anywhere (a scoring thread, the loader) ends the process, silently.
    // The coordinator's crash isolation then sees a container death, never a
    // failed request on a live instance (tiny_http answers a dropped request 500).
    std::panic::set_hook(Box::new(|_| std::process::abort()));
    limits::no_core_dumps()?;
    // The heads runtime this binary compiles must be the release runtime, and the
    // span tower runtime the one landed with spans-S2.
    ServedProfile::release_candidate()?.check_heads_runtime(&a_container_serve::HEADS_RUNTIME_SOURCES)?;
    a_container_serve::check_tower_runtime()?;
    // Only this process's artifact may be on disk: remove what an earlier
    // process in the same container left (its parts and assembled files), never
    // anything else under the work directory.
    for sub in ["blobs", "files"] {
        let dir = cfg.work.join(sub);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("clearing {sub}/ in the work directory"))?;
        }
    }
    fs::create_dir_all(cfg.work.join("blobs"))?;
    let server = tiny_http::Server::http((cfg.host.as_str(), cfg.port)).map_err(|e| anyhow!("bind: {e}"))?;
    eprintln!("a-scorer listening on {} (sessions {}, threads {})", cfg.port, cfg.sessions, cfg.threads);
    let build = json!({
        "binary_sha256": std::env::current_exe().ok().and_then(|p| file_sha256(&p)),
        "ort_sha256": file_sha256(std::path::Path::new(&cfg.lib)),
        "sessions": cfg.sessions,
        "threads": cfg.threads,
        "limits": limits::report(cfg.sessions),
    });
    let state = Arc::new(State {
        slot: Mutex::new(Slot::Empty),
        bound_parts: Mutex::new(Default::default()),
        start: Instant::now(),
        scored: AtomicUsize::new(0),
        build,
    });
    let inflight = Arc::new(AtomicUsize::new(0));
    for mut req in server.incoming_requests() {
        if inflight.fetch_add(1, SeqCst) >= MAX_INFLIGHT {
            inflight.fetch_sub(1, SeqCst);
            respond(req, 503, &json!({"error": "busy"}));
            continue;
        }
        let (cfg, state, inflight) = (cfg.clone(), state.clone(), inflight.clone());
        std::thread::spawn(move || {
            let url = req.url().to_string();
            let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
            let path = path.to_string();
            let query = query.to_string();
            let method = req.method().clone();
            use tiny_http::Method::*;
            match (method, path.as_str()) {
                (Get, "/healthz") => {
                    let v = slot_json(&state);
                    respond(req, 200, &v);
                }
                (Get, "/info") => {
                    let cpu = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
                    let first = |k: &str| {
                        cpu.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_once(':')).map(|x| x.1.trim().to_string())
                    };
                    respond(req, 200, &json!({"server": "a-scorer 0.1.0", "sessions": cfg.sessions,
                        "intra_threads": cfg.threads, "cpu_model": first("model name"),
                        "processors": cpu.lines().filter(|l| l.starts_with("processor")).count(),
                        "kernel": read_trim("/proc/version"), "slot": slot_json(&state)}));
                }
                (Head | Get, p) if p.starts_with("/v1/blobs/") => {
                    let sha = &p["/v1/blobs/".len()..];
                    let ok = artifact::is_digest(sha)
                        && (blob_path(&cfg, sha).exists() || state.bound_parts.lock().unwrap().contains(sha));
                    respond(req, if ok { 200 } else { 404 }, &json!({"present": ok}));
                }
                (Put, p) if p.starts_with("/v1/blobs/") => {
                    let sha = p["/v1/blobs/".len()..].to_string();
                    let put = PartPut::decide(
                        state.slot.lock().unwrap().bound(),
                        &state.bound_parts.lock().unwrap(),
                        &sha,
                    );
                    if put == PartPut::AlreadyBound {
                        // Already part of the bound package: nothing to store.
                        respond(req, 201, &json!({"stored": 0, "bound": true}));
                    } else if put == PartPut::Refuse {
                        respond(req, 409, &json!({"error": "instance_bound"}));
                    } else if !artifact::is_digest(&sha) {
                        respond(req, 400, &json!({"error": "digest"}));
                    } else {
                        let t = Instant::now();
                        match put_blob(&cfg, &sha, req.as_reader()) {
                            Ok(bytes) => respond(req, 201, &json!({"stored": bytes,
                                "ms": t.elapsed().as_secs_f64() * 1e3})),
                            Err(e) => respond(req, 422, &json!({"error": format!("{e:#}")})),
                        }
                    }
                }
                (Post, "/v1/load") => {
                    // A malformed query, or a spec this build does not admit by: neither is the artifact's fault.
                    let want = match load_query(&query) {
                        Ok(want) => want,
                        Err(e) => {
                            respond(req, 400, &json!({"error": e.code()}));
                            inflight.fetch_sub(1, SeqCst);
                            return;
                        }
                    };
                    let (package, spec) = (want.package.clone(), want.spec);
                    let mut body = Vec::new();
                    let read = req.as_reader().take(MAX_MANIFEST_BYTES as u64 + 1).read_to_end(&mut body);
                    let parsed = read.map_err(anyhow::Error::from).and_then(|_| {
                        let m = ArtifactFiles::parse(&body, &package)?;
                        limits::check_manifest(&m, spec)?;
                        Ok(m)
                    });
                    let bound = state.slot.lock().unwrap().bound().cloned();
                    match parsed {
                        // A bound instance answers for its own binding only.
                        _ if Load::decide(bound.as_ref(), &want) == Load::Bound => {
                            respond(req, 409, &json!({"error": "instance_bound"}))
                        }
                        Err(e) => respond(req, 422, &json!({"error": format!("{e:#}")})),
                        Ok(m) if bound.is_none()
                            && !limits::manifest_fits(&m, spec, limits::memory_limit(), cfg.sessions) =>
                        {
                            // Every admitted artifact of the class needs at most its bound: a platform fault.
                            respond(req, 507, &json!({"error": "insufficient_memory",
                                "limits": limits::report(cfg.sessions)}))
                        }
                        Ok(manifest) => {
                            let missing: Vec<String> = manifest
                                .parts()
                                .into_iter()
                                .filter(|p| !blob_path(&cfg, &p.sha256).exists())
                                .map(|p| p.sha256.clone())
                                .collect();
                            let mut slot = state.slot.lock().unwrap();
                            match Load::decide(slot.bound(), &want) {
                                Load::Same => {
                                    drop(slot);
                                    respond(req, 200, &slot_json(&state));
                                }
                                Load::Bound => {
                                    drop(slot);
                                    respond(req, 409, &json!({"error": "instance_bound"}));
                                }
                                Load::Fresh if !missing.is_empty() => {
                                    drop(slot);
                                    respond(req, 409, &json!({"error": "missing_parts", "missing": missing}));
                                }
                                Load::Fresh => {
                                    *slot = Slot::Loading { bound: want.clone(), since: Instant::now() };
                                    *state.bound_parts.lock().unwrap() =
                                        manifest.parts().into_iter().map(|p| p.sha256.clone()).collect();
                                    drop(slot);
                                    let (cfg2, state2) = (cfg.clone(), state.clone());
                                    std::thread::spawn(move || {
                                        let t = Instant::now();
                                        let r = load(&cfg2, &package, spec, &manifest);
                                        let mut slot = state2.slot.lock().unwrap();
                                        *slot = match r {
                                            Ok(model) => {
                                                eprintln!("artifact loaded");
                                                Slot::Ready { bound: want, model: Arc::new(model),
                                                    load_ms: t.elapsed().as_secs_f64() * 1e3 }
                                            }
                                            Err(e) => {
                                                eprintln!("artifact load failed");
                                                Slot::Failed { bound: want, error: format!("{e:#}") }
                                            }
                                        };
                                    });
                                    respond(req, 202, &slot_json(&state));
                                }
                            }
                        }
                    }
                }
                (Post, "/v1/score") => {
                    let mut body = Vec::new();
                    let ok = req.as_reader().take(MAX_SCORE_BODY as u64 + 1).read_to_end(&mut body).is_ok();
                    if !ok || body.len() > MAX_SCORE_BODY {
                        respond(req, 413, &json!({"error": "body"}));
                    } else {
                        match score(&cfg, &state, &body) {
                            Ok((code, v)) => respond(req, code, &v),
                            Err(e) => respond(req, 400, &json!({"error": format!("{e:#}")})),
                        }
                    }
                }
                _ => respond(req, 404, &json!({"error": "not found"})),
            }
            inflight.fetch_sub(1, SeqCst);
        });
    }
    Ok(())
}

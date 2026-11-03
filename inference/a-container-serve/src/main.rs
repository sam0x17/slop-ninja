//! a-container-serve: f32 banded ONNX serving of the baseline release A in a Container.
//!
//! Text in, release-contract prediction out. Tokenizer: the release `tokenizer.json`
//! (CLS...SEP, no truncation, inputs over 2,048 tokens are `unsupported_length`).
//! Encoder: the banded f32 ONNX graph through ONNX Runtime (graph optimisation level 3,
//! one inter-op thread, batch 1, padded to a multiple of 64 with mask 0); only its `h`
//! output feeds the heads. Heads, calibration and attribution: the release f64 heads
//! runtime (`full_a.rs`, `full_a_spans.rs`, `involvement_calibration.rs`, `shared.rs`,
//! compiled from `grammar-a-baseline` and checked against the served profile's digests).
//! The served R0 package has no token head, so no sentence or span is returned.
//!
//! Every served file is checked against the tracked served profile. The small files are
//! hashed before loading; the graph is hashed in parallel with session creation, and the
//! server reports ready only when both have finished and the graph digest matches.
//!
//! The process listens at once and loads the model in the background, so time to first
//! response and time to model ready can be measured separately. No text, token ids or
//! request ids are logged; responses carry digests and counts only, plus the prediction.
//!
//! Environment: A_HOST (0.0.0.0), A_PORT (8080), A_SESSIONS (1), A_THREADS (available
//! parallelism), A_MODEL_DIR (/model), A_ORT_LIB (/opt/ort/libonnxruntime.so.1.31.1).
use a_container_serve::full_a::FullAHeads;
use a_container_serve::profile::{ServedProfile, sha256_hex as sha};
use anyhow::{Context, Result, anyhow, ensure};
use ort::session::{Session, builder::GraphOptimizationLevel};
use ort::value::Tensor;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokenizers::Tokenizer;

// The graph shape this server implements; the served profile must agree.
const BLOCK: usize = 64;
const MAX_TOKENS: usize = 2048;
const MAX_BODY: usize = 4 << 20;
const MAX_INFLIGHT: usize = 32;

fn epoch_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1e3).unwrap_or(0.)
}

struct Config {
    host: String,
    port: u16,
    sessions: usize,
    threads: usize,
    model_dir: PathBuf,
    lib: String,
    profile: ServedProfile,
}

struct Model {
    tokenizer: Tokenizer,
    heads: FullAHeads,
    pool: Mutex<Vec<Session>>,
    free: Condvar,
    load: Value,
}

#[derive(Default)]
struct State {
    model: OnceLock<std::result::Result<Arc<Model>, String>>,
    /// The graph's digest check: Ok(sha256) when it matches the served profile.
    graph: OnceLock<std::result::Result<String, String>>,
}

impl State {
    /// The loaded model, once the graph digest has also been verified.
    fn ready(&self) -> std::result::Result<Option<Arc<Model>>, String> {
        if let Some(Err(e)) = self.graph.get() {
            return Err(e.clone());
        }
        match (self.model.get(), self.graph.get()) {
            (Some(Err(e)), _) => Err(e.clone()),
            (Some(Ok(m)), Some(Ok(_))) => Ok(Some(m.clone())),
            _ => Ok(None),
        }
    }
}

struct Clock {
    start: Instant,
    start_epoch_ms: f64,
    listening_ms: OnceLock<f64>,
    ready_ms: OnceLock<f64>,
}

fn guard(dir: &Path, profile: &ServedProfile) -> Result<(Vec<u8>, Value)> {
    for f in profile.support_files() {
        profile.check_file(dir, &f.name)?;
    }
    profile.check_heads_runtime(&a_container_serve::HEADS_RUNTIME_SOURCES)?;
    let hb = fs::read(dir.join("heads.json"))?;
    let cb = fs::read(dir.join("contract.json"))?;
    let w: Value = serde_json::from_slice(&hb)?;
    let c: Value = serde_json::from_slice(&cb)?;
    let m: Value = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    let cal: Value = serde_json::from_slice(&fs::read(dir.join("calibration.json"))?)?;
    let ch = sha(&cb);
    // The release runner's export, contract and calibration guards (full_a.rs guard,
    // guard_calibration), minus the parent safetensors check: the encoder is the ONNX graph.
    ensure!(
        m["schema"] == "slop_ninja_full_a_export_v2" && m["complete"] == true && m["calibrated"] == true,
        "incomplete or uncalibrated export"
    );
    ensure!(
        c["schema"] == "slop_ninja_full_a_contract_v2"
            && c["input_mode"] == "text"
            && c["raw_dimension"] == 768
            && c["maximum_tokens_including_specials"] == 2048
            && c["token_truncation"] == false,
        "input meaning differs"
    );
    ensure!(
        c["maximum_tokens_including_specials"] == profile.graph.max_tokens_including_specials,
        "graph and contract token limits differ"
    );
    ensure!(
        w["contract_sha256"] == ch.as_str() && w["base_temperature"] == profile.heads_base_temperature,
        "heads/contract differ"
    );
    ensure!(
        cal["schema"] == "slop_ninja_full_a_calibration_v2"
            && cal["complete"] == true
            && cal["fit_split"] == "cal"
            && cal["arm"] == m["arm"]
            && cal["seed"] == m["seed"]
            && cal["selected_checkpoint_sha256"] == m["selected_checkpoint_sha256"]
            && cal["contract_sha256"] == ch.as_str()
            && cal["involvement"]["selected"] == w["involvement_calibration"],
        "calibration/checkpoint/involvement map mismatch"
    );
    for role in ["human", "ai"] {
        let r = &cal["roles"][role];
        let empty = r["roster_empty"] == true
            && w["profiles"][role].as_array().is_some_and(|a| a.is_empty())
            && w["thresholds"][role] == 1.0;
        ensure!(
            empty || (r["qualified"] == true && r["threshold"] == w["thresholds"][role]),
            "unqualified or changed role threshold"
        );
        ensure!(w["profiles"][role] == c[format!("{role}_profiles")], "profile roster differs");
    }
    ensure!(w["span_head"].is_null(), "span-head arms need token states; not served here");
    Ok((hb, json!({"profile_id": profile.profile_id, "arm": m["arm"], "seed": m["seed"],
        "calibration": cal["involvement"]["selected"]["method"], "ai_threshold": w["thresholds"]["ai"],
        "contract_sha256": ch})))
}

fn load(cfg: &Config) -> Result<Model> {
    let t = Instant::now();
    let (hb, meta) = guard(&cfg.model_dir, &cfg.profile)?;
    let guard_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    ort::init_from(&cfg.lib).commit()?;
    let mut tokenizer =
        Tokenizer::from_file(cfg.model_dir.join("tokenizer.json")).map_err(|e| anyhow!("tokenizer: {e}"))?;
    tokenizer.with_truncation(None).map_err(|e| anyhow!("{e}"))?;
    tokenizer.with_padding(None);
    let heads = FullAHeads::from_json(&hb)?;
    let support_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let model = cfg.model_dir.join(&cfg.profile.graph.file);
    let mut pool = Vec::new();
    for _ in 0..cfg.sessions {
        pool.push(
            Session::builder()?
                .with_optimization_level(GraphOptimizationLevel::Level3)?
                .with_intra_threads(cfg.threads)?
                .with_inter_threads(1)?
                .commit_from_file(&model)?,
        );
    }
    let sessions_s = t.elapsed().as_secs_f64();
    // One untimed warm-up forward per session (a 64-token synthetic id sequence).
    let t = Instant::now();
    let warm: Vec<u32> = std::iter::once(50281).chain((0..62).map(|i| 1000 + i)).chain([50282]).collect();
    for s in pool.iter_mut() {
        forward(s, &warm)?;
    }
    let warm_s = t.elapsed().as_secs_f64();
    Ok(Model {
        tokenizer,
        heads,
        pool: Mutex::new(pool),
        free: Condvar::new(),
        load: json!({"guard_s": guard_s, "tokenizer_heads_ort_init_s": support_s, "sessions_s": sessions_s,
            "warmup_s": warm_s, "release": meta}),
    })
}

struct Graph {
    h: Vec<f32>,
    origin_logits: Vec<f32>,
    involvement_logit: f32,
    cms: f32,
    ai_logits: Vec<f32>,
}

fn forward(s: &mut Session, ids: &[u32]) -> Result<Graph> {
    let n = ids.len();
    ensure!(n > 0 && n <= MAX_TOKENS, "over-limit input ({n} tokens)");
    let m = n.div_ceil(BLOCK) * BLOCK;
    let mut x = vec![0i64; m];
    let mut mask = vec![0i64; m];
    for (i, &t) in ids.iter().enumerate() {
        x[i] = t as i64;
        mask[i] = 1;
    }
    let xt = Tensor::from_array(([1usize, m], x.into_boxed_slice()))?;
    let mt = Tensor::from_array(([1usize, m], mask.into_boxed_slice()))?;
    let o = s.run(ort::inputs!["input_ids" => xt, "attention_mask" => mt])?;
    let get = |k: &str| -> Result<Vec<f32>> {
        let (_, d) = o[k].try_extract_tensor::<f32>()?;
        Ok(d.to_vec())
    };
    let g = Graph {
        h: get("h")?,
        origin_logits: get("origin_logits")?,
        involvement_logit: get("involvement_logit")?[0],
        cms: get("conditional_mixed_share")?[0],
        ai_logits: get("ai_logits")?,
    };
    ensure!(g.h.len() == 768 && g.h.iter().all(|v| v.is_finite()), "invalid h");
    Ok(g)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: String,
    text: String,
    #[serde(default)]
    diagnostics: bool,
}

fn predict(model: &Model, profile_id: &str, body: &[u8], clock: &Clock) -> Result<Value> {
    let t0 = Instant::now();
    let r: Request = serde_json::from_slice(body).context("request JSON")?;
    ensure!(!r.id.is_empty() && r.id.len() <= 128, "id length");
    let text_sha = sha(r.text.as_bytes());
    if r.text.trim().is_empty() {
        return Ok(json!({"id": r.id, "text_sha256": text_sha, "status": "unsupported_input",
            "reason": "empty_text", "prediction": null, "calibrated": true}));
    }
    let enc = model.tokenizer.encode(r.text.as_str(), true).map_err(|e| anyhow!("tokenize: {e}"))?;
    let ids = enc.get_ids().to_vec();
    let tok_ms = t0.elapsed().as_secs_f64() * 1e3;
    let ids_sha = sha(&ids.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    if ids.len() > MAX_TOKENS {
        return Ok(json!({"id": r.id, "text_sha256": text_sha, "status": "unsupported_input",
            "reason": "unsupported_length", "token_count": ids.len(), "token_ids_sha256_le_u32": ids_sha,
            "prediction": null, "calibrated": true}));
    }
    let tq = Instant::now();
    let mut session = {
        let mut pool = model.pool.lock().unwrap();
        loop {
            if let Some(s) = pool.pop() {
                break s;
            }
            pool = model.free.wait(pool).unwrap();
        }
    };
    let queue_ms = tq.elapsed().as_secs_f64() * 1e3;
    let te = Instant::now();
    let g = forward(&mut session, &ids);
    let enc_ms = te.elapsed().as_secs_f64() * 1e3;
    model.pool.lock().unwrap().push(session);
    model.free.notify_one();
    let g = g?;
    let th = Instant::now();
    let h: Vec<f64> = g.h.iter().map(|&v| v as f64).collect();
    let out = model.heads.predict(&h, &[], &[], &[])?;
    let heads_ms = th.elapsed().as_secs_f64() * 1e3;
    let mut v = json!({"id": r.id, "profile_id": profile_id, "text_sha256": text_sha, "status": "ok",
        "token_count": ids.len(),
        "padded_to": ids.len().div_ceil(BLOCK) * BLOCK, "token_ids_sha256_le_u32": ids_sha,
        "prediction": out.prediction, "calibrated": true, "truncation": false,
        "timing_ms": {"tokenize": tok_ms, "queue": queue_ms, "encoder": enc_ms, "heads": heads_ms,
            "total": t0.elapsed().as_secs_f64() * 1e3},
        "process_uptime_ms": clock.start.elapsed().as_secs_f64() * 1e3});
    if r.diagnostics {
        v["diagnostics"] = json!({"raw": out.raw, "h": g.h, "graph": {"origin_logits": g.origin_logits,
            "involvement_logit": g.involvement_logit, "conditional_mixed_share": g.cms, "ai_logits": g.ai_logits}});
    }
    Ok(v)
}

fn read_trim(p: &str) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn status_kib(key: &str) -> Option<u64> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    s.lines().find(|l| l.starts_with(key))?.split_whitespace().nth(1)?.parse().ok()
}

fn info(cfg: &Config, state: &State, clock: &Clock) -> Value {
    let cpu = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let first = |k: &str| {
        cpu.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_once(':')).map(|x| x.1.trim().to_string())
    };
    let mhz: Vec<f64> = cpu
        .lines()
        .filter(|l| l.starts_with("cpu MHz"))
        .filter_map(|l| l.split_once(':')?.1.trim().parse().ok())
        .collect();
    let mem = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let meminfo: serde_json::Map<String, Value> = mem
        .lines()
        .filter(|l| ["MemTotal", "MemAvailable", "MemFree", "Cached", "SwapTotal"].iter().any(|k| l.starts_with(k)))
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            Some((k.to_string(), json!(v.trim())))
        })
        .collect();
    let model = match (state.ready(), state.model.get()) {
        (Ok(Some(m)), _) => json!({"ready": true, "load": m.load}),
        (Err(e), _) => json!({"ready": false, "error": e}),
        (Ok(None), Some(Ok(m))) => json!({"ready": false, "load": m.load, "waiting_for": "graph digest"}),
        (Ok(None), _) => json!({"ready": false}),
    };
    let graph = cfg.profile.graph_file().ok();
    json!({
        "server": "a-container-serve 0.1.0", "ort_crate": "2.0.0-rc.10", "ort_library": cfg.lib,
        "sessions": cfg.sessions, "intra_threads": cfg.threads,
        "available_parallelism": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "profile_id": cfg.profile.profile_id, "model": model,
        "onnx_sha256": state.graph.get().and_then(|g| g.as_ref().ok()),
        "onnx_expected_sha256": graph.map(|g| g.sha256.as_str()),
        "clock": {"process_start_epoch_ms": clock.start_epoch_ms, "listening_ms": clock.listening_ms.get(),
            "ready_ms": clock.ready_ms.get(), "uptime_ms": clock.start.elapsed().as_secs_f64() * 1e3,
            "proc_uptime": read_trim("/proc/uptime")},
        "cpu": {"model_name": first("model name"), "vendor": first("vendor_id"), "family": first("cpu family"),
            "model": first("model"), "stepping": first("stepping"), "microcode": first("microcode"),
            "cache_size": first("cache size"), "flags": first("flags"), "bugs": first("bugs"),
            "siblings": first("siblings"), "cpu_cores": first("cpu cores"),
            "processors": cpu.lines().filter(|l| l.starts_with("processor")).count(),
            "mhz": mhz, "address_sizes": first("address sizes")},
        "memory": {"meminfo": meminfo, "vm_rss_kib": status_kib("VmRSS:"), "vm_hwm_kib": status_kib("VmHWM:"),
            "cgroup_memory_max": read_trim("/sys/fs/cgroup/memory.max"),
            "cgroup_memory_current": read_trim("/sys/fs/cgroup/memory.current"),
            "cgroup_memory_peak": read_trim("/sys/fs/cgroup/memory.peak"),
            "cgroup_cpu_max": read_trim("/sys/fs/cgroup/cpu.max"),
            "cgroup_cpu_stat": read_trim("/sys/fs/cgroup/cpu.stat")},
        "kernel": read_trim("/proc/version"),
    })
}

fn respond(req: tiny_http::Request, code: u16, v: &Value) {
    let body = serde_json::to_vec(v).unwrap_or_default();
    let h = tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..]).unwrap();
    let _ = req.respond(tiny_http::Response::from_data(body).with_status_code(code).with_header(h));
}

fn main() -> Result<()> {
    let clock = Arc::new(Clock {
        start: Instant::now(),
        start_epoch_ms: epoch_ms(),
        listening_ms: OnceLock::new(),
        ready_ms: OnceLock::new(),
    });
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let cfg = Arc::new(Config {
        host: env("A_HOST").unwrap_or_else(|| "0.0.0.0".into()),
        port: env("A_PORT").map(|v| v.parse()).transpose()?.unwrap_or(8080),
        sessions: env("A_SESSIONS").map(|v| v.parse()).transpose()?.unwrap_or(1),
        threads: env("A_THREADS").map(|v| v.parse()).transpose()?.unwrap_or(cores),
        model_dir: env("A_MODEL_DIR").map(PathBuf::from).unwrap_or_else(|| "/model".into()),
        lib: env("A_ORT_LIB").unwrap_or_else(|| "/opt/ort/libonnxruntime.so.1.31.1".into()),
        profile: ServedProfile::release_candidate()?,
    });
    ensure!((1..=8).contains(&cfg.sessions) && (1..=16).contains(&cfg.threads), "A_SESSIONS/A_THREADS range");
    let g = &cfg.profile.graph;
    ensure!(
        g.pad_block == BLOCK
            && g.max_tokens_including_specials == MAX_TOKENS
            && g.inputs == ["input_ids", "attention_mask"],
        "served profile graph shape differs from this server"
    );
    let server = tiny_http::Server::http((cfg.host.as_str(), cfg.port)).map_err(|e| anyhow!("bind: {e}"))?;
    let _ = clock.listening_ms.set(clock.start.elapsed().as_secs_f64() * 1e3);
    eprintln!("listening on {} (sessions {}, threads {})", cfg.port, cfg.sessions, cfg.threads);
    let state = Arc::new(State::default());
    let mark_ready = |state: &State, clock: &Clock| {
        if matches!(state.ready(), Ok(Some(_))) && clock.ready_ms.set(clock.start.elapsed().as_secs_f64() * 1e3).is_ok() {
            eprintln!("model ready");
        }
    };
    {
        // Full digest of the graph, in parallel with session creation; ready waits for both.
        let (cfg, state, clock) = (cfg.clone(), state.clone(), clock.clone());
        std::thread::spawn(move || {
            let r = cfg.profile.graph_file().and_then(|g| {
                cfg.profile.check_file(&cfg.model_dir, &g.name)?;
                Ok(g.sha256.clone())
            });
            if r.is_err() {
                eprintln!("graph digest check failed");
            }
            let _ = state.graph.set(r.map_err(|e| format!("{e:#}")));
            mark_ready(&state, &clock);
        });
    }
    {
        let (cfg, state, clock) = (cfg.clone(), state.clone(), clock.clone());
        std::thread::spawn(move || {
            let r = load(&cfg).map(Arc::new).map_err(|e| format!("{e:#}"));
            if r.is_err() {
                eprintln!("model load failed");
            }
            let _ = state.model.set(r);
            mark_ready(&state, &clock);
        });
    }
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for mut req in server.incoming_requests() {
        use std::sync::atomic::Ordering::SeqCst;
        if inflight.fetch_add(1, SeqCst) >= MAX_INFLIGHT {
            inflight.fetch_sub(1, SeqCst);
            respond(req, 503, &json!({"error": "busy"}));
            continue;
        }
        let (cfg, state, clock, inflight) = (cfg.clone(), state.clone(), clock.clone(), inflight.clone());
        std::thread::spawn(move || {
            let path = req.url().split('?').next().unwrap_or("").to_string();
            let method = req.method().clone();
            match (method, path.as_str()) {
                (tiny_http::Method::Get, "/healthz") => {
                    let (ready, err) = match state.ready() {
                        Ok(m) => (m.is_some(), None),
                        Err(e) => (false, Some(e)),
                    };
                    respond(req, if ready { 200 } else { 503 }, &json!({"ready": ready, "error": err,
                        "listening_ms": clock.listening_ms.get(), "ready_ms": clock.ready_ms.get(),
                        "uptime_ms": clock.start.elapsed().as_secs_f64() * 1e3,
                        "process_start_epoch_ms": clock.start_epoch_ms}));
                }
                (tiny_http::Method::Get, "/info") => {
                    let v = info(&cfg, &state, &clock);
                    respond(req, 200, &v);
                }
                (tiny_http::Method::Post, "/v1/predict") => {
                    let model = match state.ready() {
                        Ok(Some(m)) => m,
                        Err(e) => {
                            respond(req, 500, &json!({"error": "model load failed", "detail": e}));
                            inflight.fetch_sub(1, SeqCst);
                            return;
                        }
                        Ok(None) => {
                            respond(req, 503, &json!({"error": "loading",
                                "uptime_ms": clock.start.elapsed().as_secs_f64() * 1e3}));
                            inflight.fetch_sub(1, SeqCst);
                            return;
                        }
                    };
                    let mut body = Vec::new();
                    let ok = req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body).is_ok();
                    if !ok || body.len() > MAX_BODY {
                        respond(req, 413, &json!({"error": "body"}));
                    } else {
                        match predict(&model, &cfg.profile.profile_id, &body, &clock) {
                            Ok(v) => respond(req, 200, &v),
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

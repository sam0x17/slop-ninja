//! Testnet drill proxy (shortcut T13; built only with `testnet-drills`).
//!
//! A loopback JSON-RPC WebSocket proxy between the validator and one public
//! endpoint. It forwards every frame unchanged and logs each extrinsic
//! submission (`author_submitAndWatchExtrinsic` / `author_submitExtrinsic`)
//! with the BLAKE2b-256 hash of its bytes, which is the extrinsic hash. For the
//! first `--faults` submissions it injects the selected fault:
//!
//! - `pass`: none.
//! - `kill-after-ack`: forward the submission and its acknowledgement, then
//!   SIGKILL the validator process in `--kill-unit` (a crash right after send).
//! - `refuse-after-ack`: forward the submission, drop the acknowledgement,
//!   close the connection and answer every WebSocket handshake with HTTP 429
//!   for `--refuse-secs` (a lost response, then rate limiting).
//! - `blackhole-after-send`: forward the submission, then deliver nothing from
//!   upstream on that connection for `--hold-secs` before closing it (a timeout).
//! - `down-after-ack`: forward the submission, drop the acknowledgement, close
//!   every connection and refuse every later one (endpoint failure; the
//!   validator must fail over to its next endpoint).
//! - `drop-before-send`: never forward the submission and close the connection
//!   (the signed bytes never reach a pool; recovery must prove era expiry).
//!
//! The node receives a forwarded submission exactly once. The proxy holds no
//! key and reads no journal. Logs are JSON lines on stdout.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use blake2::{Blake2b, Digest as _, digest::consts::U32};
use clap::{Parser, ValueEnum};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    Pass,
    KillAfterAck,
    RefuseAfterAck,
    BlackholeAfterSend,
    DownAfterAck,
    DropBeforeSend,
}

#[derive(Parser, Debug)]
#[command(about = "Loopback fault-injection proxy for testnet validator drills")]
struct Args {
    /// Loopback address to listen on.
    #[arg(long, default_value = "127.0.0.1:9955")]
    listen: SocketAddr,
    /// Upstream `wss://` endpoint.
    #[arg(long)]
    upstream: String,
    #[arg(long, value_enum, default_value = "pass")]
    mode: Mode,
    /// Submissions that receive the fault; later ones pass unchanged.
    #[arg(long, default_value_t = 1)]
    faults: u32,
    /// Systemd user unit whose validator process `kill-after-ack` kills.
    #[arg(long)]
    kill_unit: Option<String>,
    #[arg(long, default_value_t = 90)]
    refuse_secs: u64,
    #[arg(long, default_value_t = 120)]
    hold_secs: u64,
    #[arg(
        long,
        default_value = "slop_ninja_drill_proxy/0.1 (+https://slop.ninja)"
    )]
    user_agent: String,
    /// Log every client request (method and connection) with a timestamp.
    #[arg(long)]
    trace: bool,
}

struct State {
    args: Args,
    tls: Arc<rustls::ClientConfig>,
    faults_left: AtomicU32,
    connections: AtomicU64,
    down: AtomicBool,
    refuse_until: Mutex<Option<Instant>>,
    close_all: CancellationToken,
    /// Client JSON-RPC requests by method since the last per-minute report.
    requests: Mutex<std::collections::BTreeMap<String, u64>>,
}

impl State {
    fn take_fault(&self) -> bool {
        self.args.mode != Mode::Pass
            && self
                .faults_left
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
    }
    fn refusing(&self) -> bool {
        self.refuse_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|until| Instant::now() < until)
    }
}

fn log(event: &str, fields: Value) {
    let mut line = json!({
        "ts": humantime_now(),
        "event": event,
    });
    if let (Some(line), Value::Object(fields)) = (line.as_object_mut(), fields) {
        line.extend(fields);
    }
    println!("{line}");
}

fn humantime_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

fn submission(frame: &Value) -> Option<(&str, Option<String>)> {
    let method = frame.get("method")?.as_str()?;
    if method != "author_submitAndWatchExtrinsic" && method != "author_submitExtrinsic" {
        return None;
    }
    let hash = frame["params"][0]
        .as_str()
        .and_then(|text| hex::decode(text.strip_prefix("0x")?).ok())
        .map(|bytes| format!("0x{}", hex::encode(Blake2b::<U32>::digest(&bytes))));
    Some((method, hash))
}

/// SIGKILL the validator process (not its wrapper) in a systemd user unit.
fn kill_validator(unit: &str) -> Result<Vec<String>> {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "show", "-p", "ControlGroup", "--value", unit])
        .output()?;
    let group = String::from_utf8(output.stdout)?.trim().to_owned();
    if !group.starts_with('/') {
        bail!("unit {unit} has no control group");
    }
    let procs = std::fs::read_to_string(format!("/sys/fs/cgroup{group}/cgroup.procs"))?;
    let mut killed = Vec::new();
    for pid in procs.lines() {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim().starts_with("slop_ninja_vali") {
            std::process::Command::new("kill")
                .args(["-KILL", pid])
                .status()?;
            killed.push(pid.to_owned());
        }
    }
    Ok(killed)
}

fn tls() -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    let _ = roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(Arc::new(config))
}

#[allow(clippy::result_large_err)] // tungstenite's handshake callback signature
fn too_many_requests(_: &Request, _: Response) -> Result<Response, ErrorResponse> {
    Err(http::Response::builder()
        .status(429)
        .body(Some("drill: too many requests".into()))
        .unwrap_or_default())
}

async fn refuse_429(stream: TcpStream, conn: u64) {
    let _ = tokio_tungstenite::accept_hdr_async(stream, too_many_requests).await;
    log("refused_429", json!({"conn": conn}));
}

async fn serve(stream: TcpStream, conn: u64, state: Arc<State>) -> Result<()> {
    if state.down.load(Ordering::SeqCst) {
        drop(stream);
        log("refused_down", json!({"conn": conn}));
        return Ok(());
    }
    if state.refusing() {
        refuse_429(stream, conn).await;
        return Ok(());
    }
    let downstream = tokio_tungstenite::accept_async(stream).await?;
    let mut request = state.args.upstream.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert(http::header::USER_AGENT, state.args.user_agent.parse()?);
    let (upstream, _) = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(state.tls.clone())),
    )
    .await
    .context("upstream connect failed")?;
    log("connection", json!({"conn": conn}));
    let (mut down_send, mut down_recv) = downstream.split();
    let (mut up_send, mut up_recv) = upstream.split();
    // Response id of a faulted submission awaiting its acknowledgement.
    let mut armed: Option<Value> = None;
    let mut blackhole: Option<tokio::time::Instant> = None;
    let mut swallowed = 0u64;
    loop {
        let deadline =
            blackhole.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86_400));
        tokio::select! {
            () = state.close_all.cancelled() => {
                log("closed", json!({"conn": conn, "reason": "endpoint_down"}));
                return Ok(());
            }
            () = tokio::time::sleep_until(deadline), if blackhole.is_some() => {
                log("closed", json!({"conn": conn, "reason": "blackhole_elapsed", "swallowed_frames": swallowed}));
                return Ok(());
            }
            message = down_recv.next() => {
                let Some(message) = message else { break };
                let message = message?;
                let frame = message
                    .to_text()
                    .ok()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok());
                if let Some(frame) = &frame {
                    let calls: Vec<&Value> = match frame {
                        Value::Array(batch) => batch.iter().collect(),
                        single => vec![single],
                    };
                    let mut counts = state
                        .requests
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    for call in calls {
                        if let Some(method) = call.get("method").and_then(Value::as_str) {
                            *counts.entry(method.to_owned()).or_default() += 1;
                            if state.args.trace {
                                log("request", json!({"conn": conn, "method": method, "id": call.get("id")}));
                            }
                        }
                    }
                }
                if let Some(frame) = &frame
                    && let Some((method, hash)) = submission(frame)
                {
                    let faulted = state.take_fault();
                    log("submit_seen", json!({"conn": conn, "method": method, "id": frame["id"], "extrinsic_hash": hash, "fault": faulted.then(|| format!("{:?}", state.args.mode))}));
                    if faulted {
                        match state.args.mode {
                            Mode::DropBeforeSend => {
                                log("fault_injected", json!({"conn": conn, "mode": "drop_before_send", "extrinsic_hash": hash, "forwarded": false}));
                                let _ = down_send.close().await;
                                let _ = up_send.close().await;
                                return Ok(());
                            }
                            Mode::BlackholeAfterSend => {
                                up_send.send(message).await?;
                                log("fault_injected", json!({"conn": conn, "mode": "blackhole_after_send", "extrinsic_hash": hash, "forwarded": true, "hold_secs": state.args.hold_secs}));
                                blackhole = Some(tokio::time::Instant::now() + Duration::from_secs(state.args.hold_secs));
                                continue;
                            }
                            _ => armed = Some(frame["id"].clone()),
                        }
                    }
                }
                up_send.send(message).await?;
            }
            message = up_recv.next() => {
                let Some(message) = message else { break };
                let message = message?;
                if blackhole.is_some() {
                    swallowed += 1;
                    continue;
                }
                let acked = armed.as_ref().is_some_and(|id| {
                    message
                        .to_text()
                        .ok()
                        .and_then(|text| serde_json::from_str::<Value>(text).ok())
                        .is_some_and(|frame| frame.get("id") == Some(id) && frame.get("method").is_none())
                });
                if acked {
                    armed = None;
                    let text = message.to_text().unwrap_or_default().to_owned();
                    match state.args.mode {
                        Mode::KillAfterAck => {
                            down_send.send(message).await?;
                            let unit = state.args.kill_unit.as_deref().context("--kill-unit is required")?;
                            let killed = kill_validator(unit);
                            log("fault_injected", json!({"conn": conn, "mode": "kill_after_ack", "ack": text, "killed": killed.as_ref().ok(), "error": killed.as_ref().err().map(|e| e.to_string())}));
                            continue;
                        }
                        Mode::RefuseAfterAck => {
                            *state.refuse_until.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(Instant::now() + Duration::from_secs(state.args.refuse_secs));
                            log("fault_injected", json!({"conn": conn, "mode": "refuse_after_ack", "ack_dropped": text, "refuse_secs": state.args.refuse_secs}));
                            let _ = down_send.close().await;
                            return Ok(());
                        }
                        Mode::DownAfterAck => {
                            state.down.store(true, Ordering::SeqCst);
                            log("fault_injected", json!({"conn": conn, "mode": "down_after_ack", "ack_dropped": text}));
                            state.close_all.cancel();
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                down_send.send(message).await?;
            }
        }
    }
    log("closed", json!({"conn": conn, "reason": "peer_closed"}));
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    if !args.listen.ip().is_loopback() {
        bail!("the drill proxy listens on loopback only");
    }
    if !args.upstream.starts_with("wss://") {
        bail!("the upstream must be wss://");
    }
    if args.mode == Mode::KillAfterAck && args.kill_unit.is_none() {
        bail!("kill-after-ack needs --kill-unit");
    }
    let listener = TcpListener::bind(args.listen).await?;
    log(
        "listening",
        json!({"listen": args.listen.to_string(), "upstream": args.upstream, "mode": format!("{:?}", args.mode), "faults": args.faults}),
    );
    let state = Arc::new(State {
        faults_left: AtomicU32::new(args.faults),
        tls: tls()?,
        args,
        connections: AtomicU64::new(0),
        down: AtomicBool::new(false),
        refuse_until: Mutex::new(None),
        close_all: CancellationToken::new(),
        requests: Mutex::new(Default::default()),
    });
    // Per-minute request counts by method: the RPC budget actually used.
    let reporter = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.tick().await;
        loop {
            tick.tick().await;
            let counts = std::mem::take(
                &mut *reporter
                    .requests
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
            let total: u64 = counts.values().sum();
            log("rpc_minute", json!({"total": total, "methods": counts}));
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let conn = state.connections.fetch_add(1, Ordering::SeqCst) + 1;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = serve(stream, conn, state).await {
                log(
                    "connection_error",
                    json!({"conn": conn, "error": format!("{error:#}")}),
                );
            }
        });
    }
}

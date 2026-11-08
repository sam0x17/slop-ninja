//! A local stand-in for the public status API (`/api/v1/<network>/<mechanism>/status`):
//! one loopback listener answering every GET with the status and body the
//! test sets, so the client's reads, bounds and error handling run against
//! a real HTTP exchange. It never derives anything from the mock coordinator:
//! each test states the answer the feed would give (the feed lags the
//! coordinator by about a minute).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Default)]
pub struct ApiState {
    pub status: u16,
    pub body: String,
    /// Request paths, in order.
    pub paths: Vec<String>,
}

pub struct FakeApi {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<ApiState>>,
}

impl FakeApi {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(ApiState {
            status: 503,
            body: r#"{"schema":"slop_ninja.api.error/v1","available":false,"error":{"code":"upstream_unavailable","message":"m"}}"#.into(),
            paths: Vec::new(),
        }));
        let shared = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let state = shared.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8_192];
                    let mut have = 0;
                    loop {
                        let Ok(n) = stream.read(&mut buf[have..]).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        have += n;
                        if buf[..have].windows(4).any(|w| w == b"\r\n\r\n") || have == buf.len() {
                            break;
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..have]).to_string();
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let (status, body) = {
                        let mut s = state.lock().unwrap();
                        s.paths.push(path);
                        (s.status, s.body.clone())
                    };
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                         access-control-allow-origin: *\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { addr, state }
    }

    pub fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    pub fn answer(&self, status: u16, body: serde_json::Value) {
        let mut s = self.state.lock().unwrap();
        s.status = status;
        s.body = body.to_string();
    }

    pub fn reads(&self) -> usize {
        self.state.lock().unwrap().paths.len()
    }
}

/// A `cohort_status/v1` answer for `cohort` at `stage`, naming `rows`.
pub fn status_doc(
    cohort: u64,
    stage: &str,
    block: u64,
    rows: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "schema": "slop_ninja.api.cohort_status/v1",
        "view": "current",
        "network": "testnet",
        "chain": {"netuid": 601, "mechanism": 0},
        "generated_at": 1_793_846_400u64,
        "clock": {"confirmed_block": block, "tempo_blocks": 360, "block_secs_nominal": 12},
        "pause": {"paused": false, "class": null},
        "cohort": {"cohort": cohort, "label": format!("mock-c{cohort}"), "state": "running", "stage": stage},
        "next": null,
        "miners": {"source": if rows.is_some() { "registry" } else { "committing_holders" },
                   "complete": rows.is_some(), "rows": rows}
    })
}

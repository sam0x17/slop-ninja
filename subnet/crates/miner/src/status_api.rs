//! The public status API (`https://slop.ninja/api/`, `docs/testnet/public-layout.md`,
//! "The public status API"): unsigned, read-only JSON about each cohort and
//! each registered miner's intake state, read unauthenticated.
//!
//! The client reads `GET /api/v1/<network>/<mechanism>/status` (schema
//! `slop_ninja.api.cohort_status/v1`) and, for an earlier cohort,
//! `GET /api/v1/<network>/<mechanism>/cohorts/<label>`. Every field here is
//! optional and unknown fields are ignored: the API fills its per-miner
//! fields (`miners.rows`, `cohort_status`, `reason_class`, `challenges`) only
//! as the publisher's documents go live, and a value it does not know is
//! `null`. Nothing read here chooses an upload destination or authorizes an
//! action by itself: the client acts only on what the chain, the
//! coordinator's signed answers and its own state directory say
//! ([`crate::intake`]).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

pub const STATUS_SCHEMA: &str = "slop_ninja.api.cohort_status/v1";
pub const ERROR_SCHEMA: &str = "slop_ninja.api.error/v1";
/// The release's status API origin.
pub const DEFAULT_ORIGIN: &str = "https://slop.ninja/";

/// The answer bound the client accepts, rule `slop_ninja.api.bounds/v2` of
/// the website: a fixed part of 64 KiB plus 768 rows (256 registry rows and
/// the roster document's 512) of at most 1 KiB each. An answer over it is
/// refused, never truncated; a later API that raises the bound bumps the rule.
pub const BOUNDS_RULE: &str = "slop_ninja.api.bounds/v2";
pub const FIXED_BYTES: usize = 64 * 1024;
pub const MAX_ROWS: usize = 768;
pub const ROW_BYTES: usize = 1024;

pub const fn response_limit() -> usize {
    FIXED_BYTES + MAX_ROWS * ROW_BYTES
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiChain {
    pub genesis: Option<String>,
    pub netuid: Option<u16>,
    pub mechanism: Option<u8>,
}

/// The coordinator's latest chain observation, as the feed reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clock {
    pub confirmed_block: Option<u64>,
    pub confirmed_epoch: Option<u64>,
    pub observed_at: Option<u64>,
    pub age_secs: Option<u64>,
    pub boundary_block: Option<u64>,
    pub boundary_epoch: Option<u64>,
    pub tempo_blocks: Option<u64>,
    pub block_secs_nominal: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pause {
    pub paused: bool,
    pub class: Option<String>,
}

/// A future boundary: its earliest epoch and block and the estimated time
/// (a lower bound: a cohort can run later, never earlier).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Earliest {
    pub epoch: Option<u64>,
    pub block: Option<u64>,
    pub at_estimate: Option<u64>,
    pub at_estimate_utc: Option<String>,
}

/// A boundary point: observed (`at`) or earliest (`at_estimate`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mark {
    pub epoch: Option<u64>,
    pub block: Option<u64>,
    pub at: Option<u64>,
    pub basis: Option<String>,
    pub at_estimate: Option<u64>,
    pub at_estimate_utc: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Expected {
    pub stage_end: Option<Earliest>,
    pub close: Option<Earliest>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bounds {
    pub open: Option<Mark>,
    pub cutoff: Option<Mark>,
    pub close: Option<Mark>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdmissionProgress {
    pub admitted: Option<u64>,
    pub roster_cap: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkProgress {
    pub stage: Option<String>,
    pub pct: Option<u64>,
    pub as_of_epoch: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub admission: Option<AdmissionProgress>,
    pub work: Option<WorkProgress>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiCohort {
    pub cohort: Option<u64>,
    pub label: Option<String>,
    /// `running`, `queued`, `idle`, `unknown`; for a listed cohort `closed`,
    /// `ended` or `retired_unpublished`.
    pub state: Option<String>,
    /// The `cohort/v1` stage: `admission_open`, `admission_closing`,
    /// `generating`, ..., `closed`.
    pub stage: Option<String>,
    pub mode: Option<String>,
    pub results_public: bool,
    pub progress: Option<Progress>,
    pub expected: Option<Expected>,
    pub bounds: Option<Bounds>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiNext {
    pub cohort: Option<u64>,
    pub admission_tempos: Option<u64>,
    pub earliest_open: Option<Earliest>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AsOf {
    pub epoch: Option<u64>,
    pub block: Option<u64>,
}

/// An entrant's challenge progress while its cohort scores: counts only
/// (no score, order, challenge identity or failure detail): `null` on a row
/// that is not an admitted entrant of a running cohort, else `state`
/// `pending` (its scoring has not started; `total` is `null` until the
/// cohort's challenges are sealed), `running` or `complete`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Challenges {
    pub state: Option<String>,
    pub done: Option<u64>,
    pub total: Option<u64>,
}

impl Challenges {
    /// Whether this entry's scoring has not started.
    pub fn pending(&self) -> bool {
        self.state.as_deref() == Some("pending")
    }

    /// `(done, total)` when both are known and consistent.
    pub fn counts(&self) -> Option<(u64, u64)> {
        match (self.done, self.total) {
            (Some(done), Some(total)) if total > 0 && done <= total => Some((done, total)),
            _ => None,
        }
    }
}

/// One hotkey's row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MinerRow {
    pub uid: Option<u16>,
    pub hotkey: String,
    pub hotkey_hex: Option<String>,
    pub registration_block: Option<u64>,
    pub registered: Option<bool>,
    /// `none`, `committed`, `uploading`, `verifying`, `verified`, `refused`,
    /// `overflow`, `admitted`, `evaluating`, `scored`, `withheld`,
    /// `not_executed`; `null` when not published.
    pub cohort_status: Option<String>,
    /// With `refused`: `commitment`, `artifact`, `cutoff` or `registration`.
    pub reason_class: Option<String>,
    pub roster_position: Option<u32>,
    pub artifact_state: Option<String>,
    #[serde(alias = "challenge_progress")]
    pub challenges: Option<Challenges>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Miners {
    /// `registry` (every registered UID), `committing_holders`,
    /// `cohort_entrants` or `null`.
    pub source: Option<String>,
    /// `true` only when the list is every registered UID.
    pub complete: bool,
    pub as_of: Option<AsOf>,
    pub cohort_status_source: Option<String>,
    pub rows: Option<Vec<MinerRow>>,
}

/// `slop_ninja.api.cohort_status/v1`, as far as the client reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiStatus {
    pub schema: String,
    /// `current` or `cohort`.
    pub view: Option<String>,
    pub network: Option<String>,
    pub chain: Option<ApiChain>,
    pub generated_at: Option<u64>,
    pub clock: Option<Clock>,
    pub pause: Option<Pause>,
    pub sources_ok: Option<bool>,
    pub cohort: Option<ApiCohort>,
    pub next: Option<ApiNext>,
    pub miners: Option<Miners>,
}

impl ApiStatus {
    /// The row of `hotkey` (SS58), when the list names it.
    pub fn row(&self, hotkey: &str) -> Option<&MinerRow> {
        self.miners
            .as_ref()?
            .rows
            .as_ref()?
            .iter()
            .find(|r| r.hotkey == hotkey)
    }

    /// Whether the miner list is every registered UID, so a hotkey it does
    /// not name is not registered (at `miners.as_of`).
    pub fn registry_complete(&self) -> bool {
        self.miners.as_ref().is_some_and(|m| {
            m.complete && m.source.as_deref() == Some("registry") && m.rows.is_some()
        })
    }

    pub fn stage(&self) -> Option<&str> {
        self.cohort.as_ref()?.stage.as_deref()
    }

    pub fn paused(&self) -> bool {
        self.pause.as_ref().is_some_and(|p| p.paused)
    }

    /// The nominal block time the feed states, else Subtensor's 12 s.
    pub fn block_secs(&self) -> u64 {
        self.clock
            .as_ref()
            .and_then(|c| c.block_secs_nominal)
            .filter(|s| *s > 0)
            .unwrap_or(crate::chain::READBACK_SPACING.as_secs())
    }
}

/// The error answer (`slop_ninja.api.error/v1`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct ApiErrorBody {
    schema: String,
    error: ApiErrorInner,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct ApiErrorInner {
    code: String,
}

/// Why the status API could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Worth reading again later: a transport failure, a 5xx or 429, or the
    /// API's own `upstream_unavailable`/`upstream_invalid`/`not_enabled`.
    Transient(String),
    /// The API answered that the path names nothing (404) or refused the
    /// method; reading again will not change it.
    Refused { status: u16, code: Option<String> },
    /// The answer is not a status document this client reads.
    Invalid(String),
}

impl ReadError {
    pub fn transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(why) => write!(f, "the status API is unavailable for now: {why}"),
            Self::Refused { status, code } => write!(
                f,
                "the status API answered {status}{}",
                code.as_ref().map(|c| format!(" ({c})")).unwrap_or_default()
            ),
            Self::Invalid(why) => write!(f, "the status API's answer is not usable: {why}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// What to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Which {
    /// The current cohort (`status`).
    Current,
    /// One listed cohort, or the running one, by label (`cohorts/<label>`).
    Label(String),
}

/// A path segment the API accepts: lowercase letters, digits, `-` and `_`.
fn segment(text: &str) -> Result<&str, ReadError> {
    let ok = !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(text)
    } else {
        Err(ReadError::Invalid(format!(
            "{text:?} is not a network, mechanism or cohort label"
        )))
    }
}

/// The URL of `which` under `origin` (a base URL from
/// [`crate::client::base_url`]).
pub fn status_url(
    origin: &Url,
    network: &str,
    mechanism: &str,
    which: &Which,
) -> Result<Url, ReadError> {
    let path = match which {
        Which::Current => format!(
            "api/v1/{}/{}/status",
            segment(network)?,
            segment(mechanism)?
        ),
        Which::Label(label) => format!(
            "api/v1/{}/{}/cohorts/{}",
            segment(network)?,
            segment(mechanism)?,
            segment(label)?
        ),
    };
    origin
        .join(&path)
        .map_err(|e| ReadError::Invalid(format!("{path}: {e}")))
}

/// Reads one status document. Redirects are never followed (the client's
/// HTTP client refuses them), and the body is read up to
/// [`response_limit`].
pub async fn fetch(
    http: &reqwest::Client,
    origin: &Url,
    network: &str,
    mechanism: &str,
    which: &Which,
) -> Result<ApiStatus, ReadError> {
    let url = status_url(origin, network, mechanism, which)?;
    let mut response = http
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| ReadError::Transient(format!("GET {url}: {e}")))?;
    let status = response.status().as_u16();
    let bytes = match crate::client::read_bounded(&mut response, response_limit()).await {
        Ok(bytes) => bytes,
        Err(crate::client::ApiError::Transport(e)) => {
            return Err(ReadError::Transient(format!("GET {url}: {e}")));
        }
        Err(e) => return Err(ReadError::Invalid(format!("GET {url}: {e}"))),
    };
    decode(status, &bytes)
}

/// Judges one answer: its HTTP status and body.
pub fn decode(status: u16, bytes: &[u8]) -> Result<ApiStatus, ReadError> {
    if (200..300).contains(&status) {
        let parsed: ApiStatus = serde_json::from_slice(bytes)
            .map_err(|e| ReadError::Invalid(format!("not a cohort status document: {e}")))?;
        if parsed.schema != STATUS_SCHEMA {
            return Err(ReadError::Invalid(format!(
                "unknown schema {:?}",
                parsed.schema
            )));
        }
        if let Some(rows) = parsed.miners.as_ref().and_then(|m| m.rows.as_ref())
            && rows.len() > MAX_ROWS
        {
            return Err(ReadError::Invalid(format!(
                "{} miner rows, over the {MAX_ROWS} of {BOUNDS_RULE}",
                rows.len()
            )));
        }
        return Ok(parsed);
    }
    let code = serde_json::from_slice::<ApiErrorBody>(bytes)
        .ok()
        .filter(|b| b.schema == ERROR_SCHEMA && !b.error.code.is_empty())
        .map(|b| b.error.code);
    let transient_code = matches!(
        code.as_deref(),
        Some("upstream_unavailable" | "upstream_invalid" | "not_enabled")
    );
    if status >= 500 || status == 429 || status == 408 || transient_code {
        return Err(ReadError::Transient(format!(
            "HTTP {status}{}",
            code.map(|c| format!(" ({c})")).unwrap_or_default()
        )));
    }
    if (300..400).contains(&status) {
        return Err(ReadError::Invalid(format!(
            "HTTP {status}: a redirect, which the client never follows"
        )));
    }
    Err(ReadError::Refused { status, code })
}

/// The wait before the next read: the API's cache life is 60 s
/// (`Cache-Control: public, max-age=60`), so reading sooner returns the same
/// answer. Exposed for the watch loop.
pub const REFRESH: Duration = Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(rows: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": STATUS_SCHEMA,
            "view": "current",
            "network": "testnet",
            "chain": {"genesis": "0x00", "netuid": 601, "mechanism": 0},
            "clock": {"confirmed_block": 100, "confirmed_epoch": 3, "tempo_blocks": 360, "block_secs_nominal": 12},
            "pause": {"paused": false, "class": null},
            "cohort": {"cohort": 7, "label": "testnet-r0-7", "state": "running", "stage": "admission_open",
                       "results_public": false, "unknown_future_field": {"x": 1}},
            "miners": {"source": "registry", "complete": true, "as_of": {"epoch": 3, "block": 90},
                       "cohort_status_source": "roster_document", "rows": rows},
            "an_unknown_top_level_field": [1, 2, 3]
        }))
        .unwrap()
    }

    #[test]
    fn a_status_with_unknown_fields_and_nulls_parses() {
        let rows = serde_json::json!([
            {"uid": 1, "hotkey": "5A", "registered": true, "cohort_status": "verifying",
             "reason_class": null, "challenges": null},
            {"uid": null, "hotkey": "5B", "cohort_status": "evaluating",
             "challenges": {"state": "running", "done": 3, "total": 40}},
            {"uid": 3, "hotkey": "5C", "challenge_progress": {"done": 1, "total": 2}},
        ]);
        let status = decode(200, &doc(rows)).unwrap();
        assert!(status.registry_complete());
        assert_eq!(status.stage(), Some("admission_open"));
        assert_eq!(
            status.row("5A").unwrap().cohort_status.as_deref(),
            Some("verifying")
        );
        assert_eq!(
            status
                .row("5B")
                .unwrap()
                .challenges
                .as_ref()
                .unwrap()
                .counts(),
            Some((3, 40))
        );
        assert_eq!(
            status
                .row("5C")
                .unwrap()
                .challenges
                .as_ref()
                .unwrap()
                .counts(),
            Some((1, 2))
        );
        assert!(status.row("5Z").is_none());
        // A minimal Phase A answer (miners all null) parses too.
        let bare = serde_json::to_vec(&serde_json::json!({
            "schema": STATUS_SCHEMA, "miners": {"source": null, "complete": false, "rows": null}
        }))
        .unwrap();
        let bare = decode(200, &bare).unwrap();
        assert!(!bare.registry_complete());
        assert!(bare.row("5A").is_none());
        assert_eq!(bare.block_secs(), 12);
    }

    #[test]
    fn inconsistent_challenge_counts_are_not_shown() {
        for (done, total) in [
            (Some(5), Some(4)),
            (Some(1), Some(0)),
            (None, Some(3)),
            (Some(1), None),
        ] {
            let c = Challenges {
                state: Some("running".into()),
                done,
                total,
            };
            assert_eq!(c.counts(), None);
        }
        assert_eq!(
            Challenges {
                state: None,
                done: Some(0),
                total: Some(9)
            }
            .counts(),
            Some((0, 9))
        );
        // Pending before the seal: no total yet.
        let pending: Challenges =
            serde_json::from_str(r#"{"state": "pending", "done": 0, "total": null}"#).unwrap();
        assert!(pending.pending());
        assert_eq!(pending.counts(), None);
    }

    #[test]
    fn errors_are_classified() {
        let body = |code: &str| {
            serde_json::to_vec(
                &serde_json::json!({"schema": ERROR_SCHEMA, "available": false,
                "error": {"code": code, "message": "m"}}),
            )
            .unwrap()
        };
        assert!(
            decode(503, &body("upstream_unavailable"))
                .unwrap_err()
                .transient()
        );
        assert!(decode(503, &body("not_enabled")).unwrap_err().transient());
        assert!(
            decode(502, b"<html>bad gateway</html>")
                .unwrap_err()
                .transient()
        );
        assert!(decode(429, b"").unwrap_err().transient());
        assert_eq!(
            decode(404, &body("not_listed")).unwrap_err(),
            ReadError::Refused {
                status: 404,
                code: Some("not_listed".into())
            }
        );
        assert!(matches!(decode(308, b""), Err(ReadError::Invalid(_))));
        assert!(matches!(
            decode(200, b"{\"schema\":\"other/v1\"}"),
            Err(ReadError::Invalid(_))
        ));
        assert!(matches!(
            decode(200, b"not json"),
            Err(ReadError::Invalid(_))
        ));
        let many: Vec<_> = (0..=MAX_ROWS)
            .map(|i| serde_json::json!({"hotkey": format!("5{i}")}))
            .collect();
        assert!(matches!(
            decode(200, &doc(serde_json::Value::Array(many))),
            Err(ReadError::Invalid(_))
        ));
    }

    #[test]
    fn paths_are_built_from_checked_segments() {
        let origin = crate::client::base_url("https://slop.ninja").unwrap();
        assert_eq!(
            status_url(&origin, "testnet", "a", &Which::Current)
                .unwrap()
                .as_str(),
            "https://slop.ninja/api/v1/testnet/a/status"
        );
        assert_eq!(
            status_url(
                &origin,
                "testnet",
                "a",
                &Which::Label("testnet-r0-20".into())
            )
            .unwrap()
            .as_str(),
            "https://slop.ninja/api/v1/testnet/a/cohorts/testnet-r0-20"
        );
        for bad in ["", "../x", "a/b", "A", "x?y"] {
            assert!(
                status_url(&origin, bad, "a", &Which::Current).is_err(),
                "{bad}"
            );
            assert!(
                status_url(&origin, "testnet", "a", &Which::Label(bad.into())).is_err(),
                "{bad}"
            );
        }
        assert_eq!(response_limit(), 64 * 1024 + 768 * 1024);
    }
}

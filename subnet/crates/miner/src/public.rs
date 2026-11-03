//! The public status snapshot (`{public}/v1/status.json`), read
//! unauthenticated. The client reads only the fields below and ignores the
//! rest (`docs/testnet/admission-api.md`, section 10).
//!
//! The snapshot is unsigned, so it is shown as information only. Its
//! `coordinator` field chooses an upload destination only when the trust file
//! pins that origin (see [`crate::trust`]).

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::{Chain, NextCohort};
use slop_ninja_admission_wire::response_limit;
use url::Url;

pub const STATUS_SCHEMA: &str = "slop_ninja.public.status/v1";
pub const STATUS_PATH: &str = "v1/status.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmed {
    pub block: u64,
    #[serde(default)]
    pub epoch: Option<u64>,
    #[serde(default)]
    pub age_secs: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicAdmitting {
    pub cohort: u64,
    #[serde(default)]
    pub label: Option<String>,
    pub opened_epoch: u64,
    pub planned_cutoff_epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicStatus {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    #[serde(default)]
    pub confirmed: Option<Confirmed>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub fault: bool,
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub admitting: Option<PublicAdmitting>,
    #[serde(default)]
    pub next: Option<NextCohort>,
    #[serde(default)]
    pub coordinator: Option<String>,
    #[serde(default)]
    pub active_target: Option<String>,
    #[serde(default)]
    pub reference: Option<String>,
}

pub async fn fetch_status(http: &reqwest::Client, public: &Url) -> Result<PublicStatus> {
    let url = public.join(STATUS_PATH)?;
    let mut response = http
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    ensure!(
        response.status().is_success(),
        "GET {url}: {}",
        response.status()
    );
    let bytes = crate::client::read_bounded(&mut response, response_limit(None))
        .await
        .with_context(|| format!("GET {url}"))?;
    let status: PublicStatus = serde_json::from_slice(&bytes)
        .with_context(|| format!("{url} is not a status snapshot"))?;
    ensure!(
        status.schema == STATUS_SCHEMA,
        "unknown status schema {:?}",
        status.schema
    );
    Ok(status)
}

/// Where `epoch` lies relative to the snapshot's confirmed epoch, when the
/// snapshot reports one. One epoch is one tempo of the subnet.
fn relative(epoch: u64, confirmed: Option<u64>) -> String {
    let plural = |n: u64| {
        if n == 1 {
            "1 epoch".to_owned()
        } else {
            format!("{n} epochs")
        }
    };
    match confirmed {
        Some(now) if epoch > now => format!(" ({} after the confirmed epoch)", plural(epoch - now)),
        Some(now) if epoch < now => {
            format!(" ({} before the confirmed epoch)", plural(now - epoch))
        }
        Some(_) => " (the confirmed epoch)".to_owned(),
        None => String::new(),
    }
}

/// The `next-cohort` report.
pub fn next_cohort_report(status: &PublicStatus) -> String {
    let now = status.confirmed.as_ref().and_then(|c| c.epoch);
    let mut lines = Vec::new();
    lines.push(format!(
        "public snapshot (unsigned): network {} (subnet {}){}",
        status.network,
        status.chain.netuid,
        status
            .confirmed
            .as_ref()
            .map(|c| {
                let epoch = c.epoch.map(|e| format!(", epoch {e}")).unwrap_or_default();
                let age = c
                    .age_secs
                    .map(|a| format!(", {a} s old"))
                    .unwrap_or_default();
                format!("; confirmed block {}{epoch}{age}", c.block)
            })
            .unwrap_or_default()
    ));
    if status.fault {
        lines.push("the coordinator reports a fault; admission may be paused".into());
    }
    match &status.admitting {
        Some(admitting) => lines.push(format!(
            "admitting cohort {}{}: opened at epoch {}{}, planned cutoff at epoch {}{}. Commit for \
             cohort {} and upload before the cutoff.",
            admitting.cohort,
            admitting
                .label
                .as_ref()
                .map(|l| format!(" ({l})"))
                .unwrap_or_default(),
            admitting.opened_epoch,
            relative(admitting.opened_epoch, now),
            admitting.planned_cutoff_epoch,
            relative(admitting.planned_cutoff_epoch, now),
            admitting.cohort
        )),
        None => lines.push("no cohort is admitting now".into()),
    }
    if let Some(next) = &status.next {
        lines.push(format!(
            "next cohort {}{}. A commitment counts only inside that cohort's window.",
            next.cohort,
            next.earliest_open_epoch
                .map(|e| format!(" opens at epoch {e}{} at the earliest", relative(e, now)))
                .unwrap_or_default()
        ));
    }
    if let Some(notice) = &status.notice {
        lines.push(format!("notice: {notice}"));
    }
    lines.join("\n")
}

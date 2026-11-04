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
        let window = next.admission_tempos.map(|a| {
            let tempos = if a == 1 {
                "1 epoch".to_owned()
            } else {
                format!("{a} epochs")
            };
            match next.earliest_open_epoch {
                Some(e) => format!(
                    "; its admission window lasts {tempos} once it opens (until epoch {} at \
                     the earliest)",
                    e.saturating_add(a)
                ),
                None => format!("; its admission window lasts {tempos} once it opens"),
            }
        });
        lines.push(format!(
            "next cohort {}{}{}. A commitment counts only inside that cohort's window: one \
             made before it opens is refused.",
            next.cohort,
            next.earliest_open_epoch
                .map(|e| format!(" opens at epoch {e}{} at the earliest", relative(e, now)))
                .unwrap_or_default(),
            window.unwrap_or_default()
        ));
    }
    if let Some(notice) = &status.notice {
        lines.push(format!("notice: {notice}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(next: Option<NextCohort>) -> PublicStatus {
        serde_json::from_value(serde_json::json!({
            "schema": STATUS_SCHEMA,
            "network": "testnet",
            "chain": {"genesis": "00".repeat(32), "netuid": 601, "mechanism": 0},
            "confirmed": {"block": 8_366_973, "epoch": 98, "age_secs": 4},
            "admitting": null,
            "next": next,
        }))
        .unwrap()
    }

    /// The report gives the next cohort's earliest opening and, when the
    /// snapshot carries it, how long its admission window lasts.
    #[test]
    fn the_next_cohort_report_names_the_opening_and_the_window() {
        let with = next_cohort_report(&status(Some(NextCohort {
            cohort: 20,
            earliest_open_epoch: Some(113),
            admission_tempos: Some(17),
        })));
        assert!(with.contains("no cohort is admitting now"), "{with}");
        assert!(
            with.contains(
                "next cohort 20 opens at epoch 113 (15 epochs after the confirmed epoch) at the earliest; \
                 its admission window lasts 17 epochs once it opens (until epoch 130 at the earliest)"
            ),
            "{with}"
        );
        assert!(
            with.contains("one made before it opens is refused"),
            "{with}"
        );
        // A snapshot without the window (an older publisher) still parses.
        let without = next_cohort_report(&status(Some(NextCohort {
            cohort: 20,
            earliest_open_epoch: Some(113),
            admission_tempos: None,
        })));
        assert!(!without.contains("admission window lasts"), "{without}");
        let v = serde_json::to_value(NextCohort {
            cohort: 1,
            earliest_open_epoch: None,
            admission_tempos: None,
        })
        .unwrap();
        assert!(
            v.get("admission_tempos").is_none(),
            "absent, not null, when unknown"
        );
    }
}

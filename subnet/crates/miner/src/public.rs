//! The public status snapshot (`{public}/v1/status.json`), read
//! unauthenticated. The client reads only the fields below and ignores the
//! rest (`docs/testnet/admission-api.md`, section 10).
//!
//! The snapshot is unsigned, so it is shown as information only. Its
//! `coordinator` field chooses an upload destination only when the trust file
//! pins that origin (see [`crate::trust`]). Before the status host is live,
//! [`cohort_report`] shows the same calendar from the coordinator's unsigned
//! cohort read.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::{Chain, CohortInfo, NextCohort};
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

/// The admitting cohort as a report shows it.
struct ShownAdmitting<'a> {
    cohort: u64,
    label: Option<&'a str>,
    opened_epoch: u64,
    planned_cutoff_epoch: u64,
}

/// The calendar lines shared by both reports: the admitting cohort, or none,
/// and the next cohort, with epochs relative to `now` when it is known.
fn calendar_lines(
    lines: &mut Vec<String>,
    now: Option<u64>,
    admitting: Option<ShownAdmitting<'_>>,
    next: Option<&NextCohort>,
) {
    match admitting {
        Some(admitting) => lines.push(format!(
            "admitting cohort {}{}: opened at epoch {}{}, planned cutoff at epoch {}{}. Commit for \
             cohort {} and upload before the cutoff.",
            admitting.cohort,
            admitting
                .label
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
    if let Some(next) = next {
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
}

/// The `next-cohort` report from the public snapshot.
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
    calendar_lines(
        &mut lines,
        now,
        status.admitting.as_ref().map(|a| ShownAdmitting {
            cohort: a.cohort,
            label: a.label.as_deref(),
            opened_epoch: a.opened_epoch,
            planned_cutoff_epoch: a.planned_cutoff_epoch,
        }),
        status.next.as_ref(),
    );
    if let Some(notice) = &status.notice {
        lines.push(format!("notice: {notice}"));
    }
    lines.join("\n")
}

/// The `next-cohort` report from the coordinator's unsigned cohort read
/// (`GET /admission/v1/cohort`), for use before the public status host is
/// live. `origin` is the coordinator it was read from. Epoch distances are
/// measured from the cohort read's `confirmed_epoch`; a coordinator that does
/// not report one gets absolute epochs only.
pub fn cohort_report(info: &CohortInfo, origin: &str) -> String {
    let now = info.confirmed_epoch;
    let mut lines = vec![format!(
        "coordinator cohort read (unsigned) from {origin}: subnet {}; finalized block {}{}",
        info.chain.netuid,
        info.finalized_block,
        now.map(|e| format!(", confirmed epoch {e}")).unwrap_or_default()
    )];
    if info.paused {
        lines.push("the coordinator reports admission paused (catching up or unavailable)".into());
    }
    calendar_lines(
        &mut lines,
        now,
        info.admitting.as_ref().map(|a| ShownAdmitting {
            cohort: a.cohort,
            label: Some(a.label.as_str()),
            opened_epoch: a.opened_epoch,
            planned_cutoff_epoch: a.planned_cutoff_epoch,
        }),
        info.next.as_ref(),
    );
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
            ..Default::default()
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
            ..Default::default()
        })));
        assert!(!without.contains("admission window lasts"), "{without}");
        let v = serde_json::to_value(NextCohort {
            cohort: 1,
            earliest_open_epoch: None,
            admission_tempos: None,
            ..Default::default()
        })
        .unwrap();
        assert!(
            v.get("admission_tempos").is_none(),
            "absent, not null, when unknown"
        );
    }
    /// Before the status host is live, the coordinator's unsigned cohort read
    /// gives the same calendar, measured from its confirmed epoch.
    #[test]
    fn the_cohort_read_report_gives_the_same_calendar() {
        let mut info: CohortInfo = serde_json::from_value(serde_json::json!({
            "schema": slop_ninja_admission_wire::json::SCHEMA_COHORT,
            "contract": slop_ninja_admission_wire::CONTRACT,
            "chain": {"genesis": "00".repeat(32), "netuid": 601, "mechanism": 0},
            "finalized_block": 8_367_213,
            "request_validity_blocks": 100,
            "admitting": null,
            "next": {"cohort": 19, "earliest_open_epoch": 113, "admission_tempos": 1},
            "confirmed_epoch": 99,
        }))
        .unwrap();
        let out = cohort_report(&info, "https://testnet-admission.slop.ninja");
        assert!(
            out.starts_with(
                "coordinator cohort read (unsigned) from https://testnet-admission.slop.ninja: \
                 subnet 601; finalized block 8367213, confirmed epoch 99"
            ),
            "{out}"
        );
        assert!(out.contains("no cohort is admitting now"), "{out}");
        assert!(
            out.contains(
                "next cohort 19 opens at epoch 113 (14 epochs after the confirmed epoch) at the earliest; \
                 its admission window lasts 1 epoch once it opens (until epoch 114 at the earliest)"
            ),
            "{out}"
        );
        // A coordinator without the field (an older build): absolute epochs only.
        info.confirmed_epoch = None;
        info.paused = true;
        let out = cohort_report(&info, "https://testnet-admission.slop.ninja");
        assert!(out.contains("finalized block 8367213\n"), "{out}");
        assert!(out.contains("admission paused"), "{out}");
        assert!(
            out.contains("next cohort 19 opens at epoch 113 at the earliest"),
            "{out}"
        );
        assert!(!out.contains("confirmed epoch"), "{out}");
        // An older coordinator's response without the field still parses.
        let mut v = serde_json::to_value(&info).unwrap();
        assert!(
            v.get("confirmed_epoch").is_none(),
            "absent, not null, when unknown"
        );
        v.as_object_mut().unwrap().remove("confirmed_epoch");
        assert_eq!(serde_json::from_value::<CohortInfo>(v).unwrap(), info);
    }
}

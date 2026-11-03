//! The coordinator Workers' `/status`, as the publisher reads it, reduced to
//! what the relay checks while the publisher is silent: the RPC-confirmed
//! watermark's age, the journal's durable fault and the signing state.
//!
//! The body is parsed into typed views of the named fields only (no JSON
//! tree), within [`crate::arena::STATUS_BODY_BOUND`].

use serde::Deserialize;

/// One Worker's `/status`, reduced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerView {
    pub confirmed_age_ms: Option<u64>,
    pub fault: Option<String>,
    pub signing_fault: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServiceJson {
    chain_journal: ChainJournalJson,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ChainJournalJson {
    journal: JournalJson,
    signing: Option<SigningJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JournalJson {
    confirmed_age_ms: Option<u64>,
    fault: Option<FaultJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FaultJson {
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SigningJson {
    state: Option<String>,
}

/// Parses a `/status` body; `None` when it is not the expected shape.
pub fn parse(body: &[u8]) -> Option<WorkerView> {
    let j: ServiceJson = serde_json::from_slice(body).ok()?;
    let c = j.chain_journal;
    Some(WorkerView {
        confirmed_age_ms: c.journal.confirmed_age_ms,
        fault: c
            .journal
            .fault
            .map(|f| f.kind.unwrap_or_else(|| "fault".into())),
        signing_fault: c.signing.and_then(|s| s.state).as_deref() == Some("fault"),
    })
}

/// What one status read gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// No status token is configured for this Worker: not read.
    NoToken,
    /// No answer within the timeout, a non-200, or an unexpected body.
    Unanswered,
    Ok(WorkerView),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_fields_only() {
        let body = br#"{"service":"slop-ninja-coordinator","chain_journal":{"mode":"primary",
            "journal":{"confirmed":{"number":5},"confirmed_age_ms":4000,"fault":{"kind":"gap","detail":"x"}},
            "signing":{"state":"fault","reason":"r","ops":[]}},"eval_cohort":{"cohorts":[1,2,3]}}"#;
        assert_eq!(
            parse(body),
            Some(WorkerView {
                confirmed_age_ms: Some(4000),
                fault: Some("gap".into()),
                signing_fault: true
            })
        );
        assert_eq!(parse(b"{}"), Some(WorkerView::default()));
        assert_eq!(parse(b"not json"), None);
    }
}

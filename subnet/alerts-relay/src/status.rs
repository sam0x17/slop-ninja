//! The coordinator Workers' `/status`, as the publisher reads it, reduced to
//! what the relay checks while the publisher is silent: the RPC-confirmed
//! watermark's age, the journal's durable fault and the signing state.
//!
//! The body is parsed into typed views of the named fields only (no JSON
//! tree), within [`crate::arena::STATUS_BODY_BOUND`].
//!
//! It also holds the status check after a token change ([`CheckState`]):
//! the first tick after the relay's status tokens change reads both Workers
//! once, whether or not the publisher is silent, logs the outcome with the
//! caller each Worker recognized and keeps it in the relay's state (stated
//! in `latest.json` as `status_check`), so a token change is verified
//! without stopping the publisher and without a live log.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// One Worker's `/status`, reduced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerView {
    pub confirmed_age_ms: Option<u64>,
    pub fault: Option<String>,
    pub signing_fault: bool,
    /// The caller the Worker recognized for the relay's token (`access.caller`;
    /// absent before the status gate). Logged by the status check only; no
    /// rule reads it.
    pub caller: Option<Caller>,
}

/// The coordinator's `access.caller` values (coordinator `access::Caller`).
/// A value this build does not know parses as [`Caller::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Caller {
    Operator,
    Monitor,
    Publisher,
    Alerts,
    Shared,
    #[serde(other)]
    Other,
}

impl Caller {
    /// Every value, in declaration order (a test checks the outcome bound
    /// over them).
    pub const ALL: [Self; 6] = [
        Self::Operator,
        Self::Monitor,
        Self::Publisher,
        Self::Alerts,
        Self::Shared,
        Self::Other,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Monitor => "monitor",
            Self::Publisher => "publisher",
            Self::Alerts => "alerts",
            Self::Shared => "shared",
            Self::Other => "other",
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServiceJson {
    chain_journal: ChainJournalJson,
}

/// `access` is parsed in a pass of its own, so a value this build cannot
/// read only drops the caller and never turns a liveness read unanswered.
#[derive(Deserialize, Default)]
#[serde(default)]
struct AccessOnlyJson {
    access: Option<AccessJson>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AccessJson {
    caller: Option<Caller>,
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
        caller: serde_json::from_slice::<AccessOnlyJson>(body)
            .ok()
            .and_then(|a| a.access)
            .and_then(|a| a.caller),
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

impl Read {
    /// The status check's log word for this read: `no_token`, `unanswered`,
    /// or `ok` with the recognized caller (`ok caller=alerts`; `caller=none`
    /// before the status gate). At most [`OUTCOME_MAX_BYTES`].
    pub fn outcome(&self) -> String {
        match self {
            Self::NoToken => "no_token".into(),
            Self::Unanswered => "unanswered".into(),
            Self::Ok(w) => format!("ok caller={}", w.caller.map_or("none", Caller::name)),
        }
    }
}

/// Domain separation for [`token_fingerprint`].
const FINGERPRINT_DOMAIN: &[u8] = b"slop_ninja.alerts_relay.status_check/v1\0";

/// A digest of the relay's two status tokens as it presents them (trimmed;
/// `None` when not configured), in the order coordinator, eval Worker. The
/// relay's state keeps the fingerprint whose status check answered; a
/// different fingerprint (a token put, changed or deleted) makes the next
/// tick check again. Each token is a random secret of at least 32 bytes, so
/// the digest reveals nothing usable about it.
pub fn token_fingerprint(coordinator: Option<&str>, eval: Option<&str>) -> String {
    let mut h = Sha256::new();
    h.update(FINGERPRINT_DOMAIN);
    for token in [coordinator, eval] {
        match token {
            None => h.update([0u8]),
            Some(t) => {
                h.update([1u8]);
                h.update((t.len() as u64).to_le_bytes());
                h.update(t.as_bytes());
            }
        }
    }
    hex::encode(h.finalize())
}

/// The longest [`Read::outcome`]: `ok caller=` and the longest caller name
/// (`publisher`; a test checks every outcome against it).
pub const OUTCOME_MAX_BYTES: usize = "ok caller=".len() + "publisher".len();

/// Whether a status check settled its fingerprint: every Worker answered or
/// has no token. An unanswered read is checked again at the next tick,
/// which reads no more often than the dead-man check does while the
/// publisher is silent (two reads a tick).
fn check_settled(reads: &[Read; 2]) -> bool {
    !reads.iter().any(|r| matches!(r, Read::Unanswered))
}

/// One status check's outcome, kept in the relay's state and stated in
/// `latest.json` (`status_check`) until the next check replaces it, so it
/// can be read after the fact. Content-free: the time and each Worker's
/// [`Read::outcome`], never a token or its fingerprint.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    /// The tick that ran the check.
    pub at_ms: u64,
    pub coordinator: String,
    pub eval: String,
    /// No read was unanswered: the check will not run again until the
    /// tokens change. `false`: the next tick checks again.
    pub settled: bool,
}

/// The status check's part of the relay's state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckState {
    /// The fingerprint of the status tokens whose check settled
    /// ([`token_fingerprint`]): a different one makes the next tick check.
    #[serde(default)]
    pub settled_for: Option<String>,
    /// The last check's outcome, settled or not.
    #[serde(default)]
    pub last: Option<CheckRecord>,
}

/// What a tick reads from the Workers before it evaluates
/// ([`CheckState::plan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The fingerprint of the tokens this tick presents.
    pub fingerprint: String,
    /// The publisher is down: the rules see the reads (the dead-man check).
    pub down: bool,
    /// The tokens differ from the last settled check's: the reads are also
    /// the status check.
    pub check: bool,
}

impl Plan {
    /// Whether to read both Workers' `/status` this tick.
    pub fn read(&self) -> bool {
        self.down || self.check
    }
}

impl CheckState {
    /// The tick's plan for the tokens it presents (`fingerprint`) and the
    /// publisher's liveness.
    pub fn plan(&self, fingerprint: String, down: bool) -> Plan {
        let check = self.settled_for.as_deref() != Some(fingerprint.as_str());
        Plan {
            fingerprint,
            down,
            check,
        }
    }

    /// Takes the tick's reads (`Some` exactly when [`Plan::read`]): records
    /// the status check when the plan has one, settling its fingerprint only
    /// when no read was unanswered, and returns what the rules see, the
    /// reads only while the publisher is down. Returns the check's record
    /// for the log when one ran.
    pub fn conclude(
        &mut self,
        plan: Plan,
        reads: Option<[Read; 2]>,
        now_ms: u64,
    ) -> (Option<[Read; 2]>, Option<CheckRecord>) {
        let record = match (&reads, plan.check) {
            (Some(r), true) => {
                let record = CheckRecord {
                    at_ms: now_ms,
                    coordinator: r[0].outcome(),
                    eval: r[1].outcome(),
                    settled: check_settled(r),
                };
                if record.settled {
                    self.settled_for = Some(plan.fingerprint);
                }
                self.last = Some(record.clone());
                Some(record)
            }
            _ => None,
        };
        (reads.filter(|_| plan.down), record)
    }
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
                signing_fault: true,
                caller: None,
            })
        );
        assert_eq!(parse(b"{}"), Some(WorkerView::default()));
        assert_eq!(parse(b"not json"), None);
    }

    /// The coordinator's `/status` for the relay's token is its liveness
    /// subset (coordinator `access::alerts_journal`); it still decides the
    /// relay's rule.
    #[test]
    fn the_reduced_liveness_view_parses() {
        let body = br#"{"service":"slop-ninja-coordinator","chain_journal":{"journal":{"confirmed_age_ms":4000,
            "fault":{"kind":"gap"}},"signing":{"state":"fault"}},"chain_journal_liveness":{"verdict":"ok",
            "record":null},"eval_cohort":null,"arena":null,"access":{"caller":"alerts"}}"#;
        assert_eq!(
            parse(body),
            Some(WorkerView {
                confirmed_age_ms: Some(4000),
                fault: Some("gap".into()),
                signing_fault: true,
                caller: Some(Caller::Alerts),
            })
        );
        let kindless =
            br#"{"chain_journal":{"journal":{"fault":{"kind":null}},"signing":{"state":null}}}"#;
        assert_eq!(
            parse(kindless),
            Some(WorkerView {
                confirmed_age_ms: None,
                fault: Some("fault".into()),
                signing_fault: false,
                caller: None,
            })
        );
    }

    /// The caller each Worker recognized, as the status check logs it.
    #[test]
    fn the_caller_is_read_and_logged() {
        let with = |caller: &str| {
            parse(
                format!(r#"{{"chain_journal":{{}},"access":{{"caller":"{caller}"}}}}"#).as_bytes(),
            )
            .map(Read::Ok)
            .unwrap()
            .outcome()
        };
        assert_eq!(with("alerts"), "ok caller=alerts");
        assert_eq!(with("shared"), "ok caller=shared");
        assert_eq!(with("publisher"), "ok caller=publisher");
        assert_eq!(with("someone_new"), "ok caller=other");
        assert_eq!(Read::Ok(parse(b"{}").unwrap()).outcome(), "ok caller=none");
        assert_eq!(
            Read::Ok(parse(br#"{"access":{"caller":null}}"#).unwrap()).outcome(),
            "ok caller=none"
        );
        // An unreadable `access` drops the caller, never the liveness read.
        let odd = br#"{"chain_journal":{"journal":{"confirmed_age_ms":7}},"access":{"caller":5}}"#;
        assert_eq!(
            parse(odd),
            Some(WorkerView {
                confirmed_age_ms: Some(7),
                ..Default::default()
            })
        );
        assert_eq!(Read::NoToken.outcome(), "no_token");
        assert_eq!(Read::Unanswered.outcome(), "unanswered");
    }

    #[test]
    fn the_fingerprint_changes_with_any_token() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let base = token_fingerprint(Some(&a), Some(&b));
        assert_eq!(base.len(), 64);
        assert_eq!(base, token_fingerprint(Some(&a), Some(&b)));
        assert!(!base.contains(&a) && !base.contains(&b));
        for other in [
            token_fingerprint(Some(&b), Some(&a)),
            token_fingerprint(Some(&a), None),
            token_fingerprint(None, Some(&b)),
            token_fingerprint(None, None),
            token_fingerprint(Some(&a), Some(&a)),
        ] {
            assert_ne!(base, other);
        }
        // Lengths are framed: moving a byte across the boundary changes it.
        assert_ne!(
            token_fingerprint(Some("ab"), Some("c")),
            token_fingerprint(Some("a"), Some("bc"))
        );
        assert_ne!(
            token_fingerprint(None, Some("")),
            token_fingerprint(None, None)
        );
    }

    #[test]
    fn outcomes_fit_their_bound() {
        let mut all = vec![Read::NoToken.outcome(), Read::Unanswered.outcome()];
        all.push(Read::Ok(WorkerView::default()).outcome());
        for c in Caller::ALL {
            all.push(
                Read::Ok(WorkerView {
                    caller: Some(c),
                    ..Default::default()
                })
                .outcome(),
            );
        }
        assert!(all.iter().all(|o| o.len() <= OUTCOME_MAX_BYTES), "{all:?}");
        assert!(all.iter().any(|o| o.len() == OUTCOME_MAX_BYTES));
        assert!(all.iter().all(|o| !o.contains(['"', '\\'])));
    }

    fn alerts() -> Read {
        Read::Ok(WorkerView {
            caller: Some(Caller::Alerts),
            ..Default::default()
        })
    }

    /// The publisher is up and the tokens are the settled ones: no read, no
    /// check, nothing for the rules.
    #[test]
    fn an_unchanged_token_reads_nothing_while_the_publisher_is_up() {
        let f = token_fingerprint(Some("c"), Some("e"));
        let mut st = CheckState {
            settled_for: Some(f.clone()),
            last: None,
        };
        let plan = st.plan(f.clone(), false);
        assert!(!plan.read() && !plan.check);
        let before = st.clone();
        assert_eq!(st.conclude(plan, None, 9), (None, None));
        assert_eq!(st, before);
    }

    /// The publisher is up after a token change: both Workers are read once,
    /// the outcome is recorded and settles, the rules see nothing, and the
    /// next tick reads nothing.
    #[test]
    fn a_changed_token_is_checked_once_while_the_publisher_is_up() {
        let old = token_fingerprint(Some("c"), Some("e"));
        let new = token_fingerprint(Some("c2"), Some("e2"));
        let mut st = CheckState {
            settled_for: Some(old),
            last: None,
        };
        let plan = st.plan(new.clone(), false);
        assert!(plan.read() && plan.check);
        let (workers, record) = st.conclude(plan, Some([alerts(), alerts()]), 7);
        assert_eq!(
            workers, None,
            "the rules see no read while the publisher is up"
        );
        let record = record.unwrap();
        assert_eq!(
            (record.coordinator.as_str(), record.eval.as_str()),
            ("ok caller=alerts", "ok caller=alerts")
        );
        assert!(record.settled && record.at_ms == 7);
        assert_eq!(st.last.as_ref(), Some(&record));
        assert_eq!(st.settled_for.as_deref(), Some(new.as_str()));
        let next = st.plan(new, false);
        assert!(!next.read());
        // A build without a stored fingerprint (the first deploy of the
        // check) checks too.
        let plan = CheckState::default().plan(token_fingerprint(None, None), false);
        assert!(plan.read() && plan.check);
    }

    /// An unanswered read while the publisher is up: recorded unsettled, the
    /// rules untouched, and the next tick checks again; an answer then
    /// settles it and replaces the record.
    #[test]
    fn an_unanswered_check_is_retried_and_never_reaches_the_rules() {
        let f = token_fingerprint(Some("c"), Some("e"));
        let mut st = CheckState::default();
        let plan = st.plan(f.clone(), false);
        let (workers, record) = st.conclude(plan, Some([alerts(), Read::Unanswered]), 1);
        assert_eq!(workers, None);
        let record = record.unwrap();
        assert!(!record.settled && record.eval == "unanswered");
        assert_eq!(st.settled_for, None);
        assert_eq!(st.last.as_ref(), Some(&record));
        let plan = st.plan(f.clone(), false);
        assert!(plan.read() && plan.check, "checked again next tick");
        let (workers, record) = st.conclude(plan, Some([alerts(), alerts()]), 2);
        assert_eq!(workers, None);
        assert!(record.unwrap().settled);
        assert_eq!(st.settled_for.as_deref(), Some(f.as_str()));
        assert_eq!(st.last.as_ref().map(|r| r.at_ms), Some(2));
    }

    /// The publisher is down: the rules see the reads, with or without a
    /// check in the same tick (one pair of reads serves both).
    #[test]
    fn reads_reach_the_rules_only_while_the_publisher_is_down() {
        let f = token_fingerprint(Some("c"), Some("e"));
        let mut st = CheckState {
            settled_for: Some(f.clone()),
            last: None,
        };
        let plan = st.plan(f.clone(), true);
        assert!(plan.read() && !plan.check);
        let reads = [alerts(), Read::Unanswered];
        let (workers, record) = st.conclude(plan, Some(reads.clone()), 3);
        assert_eq!(workers.as_ref(), Some(&reads));
        assert_eq!(record, None);
        assert_eq!(st.last, None);
        // Down and a token change at once: the reads go to the rules and to
        // the check.
        let g = token_fingerprint(Some("c2"), Some("e"));
        let plan = st.plan(g.clone(), true);
        assert!(plan.read() && plan.check);
        let (workers, record) = st.conclude(plan, Some(reads.clone()), 4);
        assert_eq!(workers.as_ref(), Some(&reads));
        assert!(!record.unwrap().settled);
        assert_eq!(st.settled_for.as_deref(), Some(f.as_str()));
    }

    #[test]
    fn a_check_settles_once_nothing_is_unanswered() {
        let ok = || Read::Ok(WorkerView::default());
        assert!(check_settled(&[ok(), ok()]));
        assert!(check_settled(&[ok(), Read::NoToken]));
        assert!(check_settled(&[Read::NoToken, Read::NoToken]));
        assert!(!check_settled(&[ok(), Read::Unanswered]));
        assert!(!check_settled(&[Read::Unanswered, Read::NoToken]));
    }
}

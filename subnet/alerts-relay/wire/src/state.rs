//! Alert state across runs: holds, reminders and resolutions.
//!
//! Each run turns what it observed into [`Condition`]s. [`observe`] records
//! when each condition was first seen; [`due`] lists the posts due now: a
//! firing once the condition has held for its hold time, a reminder every
//! renotify interval while it lasts, and a resolution once it clears after it
//! fired; [`attempt`] marks the posts about to be sent, and [`acknowledge`]
//! marks delivered posts. A failed post leaves its event due (with the same
//! id), so the next run retries it; a duplicate run posts nothing new.
//!
//! A post whose answer is lost may still have been sent:
//!
//! - A firing marked attempted is never dropped silently: when it clears, a
//!   resolution is due like for a notified entry (with no `notified_ms`),
//!   and the relay emails that resolution only when the firing may have gone
//!   out (its sent-event ledger knows). Only a receiver with such a ledger
//!   gets firing marks ([`attempt`]); a plain one gets [`attempt_resolutions`].
//! - A resolution marked attempted may already be in the mailbox. If the
//!   condition comes back before the resolution is acknowledged, a new
//!   episode starts (a new first-seen time, so a new firing id after the
//!   hold) instead of reviving the old one, whose next post would only be a
//!   reminder one renotify interval later.
//!
//! The publisher keeps this state in its Durable Object, the relay in its own
//! (the serialized shape is the publisher's `alerts/v2` shape).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{Event, EventKind, event_id};

/// One condition observed this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Condition {
    pub rule: &'static str,
    pub subject: String,
    pub detail: String,
    pub hold_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub rule: String,
    pub subject: String,
    pub detail: String,
    pub hold_ms: u64,
    pub first_seen_ms: u64,
    /// The last firing or reminder post, once it fired.
    pub notified_ms: Option<u64>,
    /// Cleared after it fired (or after a firing post was attempted); the
    /// resolution is not yet posted.
    pub resolved: bool,
    /// The first firing post attempted for it whose answer did not
    /// acknowledge it (the post failed, timed out or its answer was lost).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempted_ms: Option<u64>,
    /// The first resolution post attempted for it whose answer did not
    /// acknowledge it: the resolution may be in the mailbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_attempted_ms: Option<u64>,
}

impl Entry {
    fn new(rule: &str, subject: &str, now_ms: u64) -> Self {
        Self {
            rule: rule.into(),
            subject: subject.into(),
            first_seen_ms: now_ms,
            ..Self::default()
        }
    }
}

/// Alert state across runs, keyed by `rule/subject`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertState {
    /// The rule set that last observed this state; it also scopes the event
    /// ids.
    pub rules: String,
    pub entries: BTreeMap<String, Entry>,
}

pub fn key(rule: &str, subject: &str) -> String {
    format!("{rule}/{subject}")
}

impl AlertState {
    /// The entry of `rule/subject`, if any.
    pub fn get(&self, rule: &str, subject: &str) -> Option<&Entry> {
        self.entries.get(&key(rule, subject))
    }

    /// The entries that cleared after they fired and await their resolution
    /// post.
    pub fn resolving_mut(&mut self) -> impl Iterator<Item = &mut Entry> {
        self.entries.values_mut().filter(|e| e.resolved)
    }
}

/// Records this run's conditions under `rules`: new ones start their hold,
/// cleared ones that fired (or whose firing post was attempted) await their
/// resolution post, cleared ones that never fired vanish. A condition that
/// comes back before its resolution is acknowledged is still firing, unless
/// that resolution was attempted (it may be in the mailbox): then a new
/// episode starts.
pub fn observe(state: &mut AlertState, rules: &str, conditions: &[Condition], now_ms: u64) {
    rules.clone_into(&mut state.rules);
    let mut present = BTreeSet::new();
    for c in conditions {
        let k = key(c.rule, &c.subject);
        present.insert(k.clone());
        let e = state
            .entries
            .entry(k)
            .or_insert_with(|| Entry::new(c.rule, &c.subject, now_ms));
        if e.resolved {
            if e.resolution_attempted_ms.is_some() {
                // Its resolution may have gone out: a new episode, which
                // fires again (with a new id) after its hold.
                *e = Entry::new(c.rule, &c.subject, now_ms);
            } else {
                // It came back before its resolution was posted: still
                // firing.
                e.resolved = false;
            }
        }
        e.detail.clone_from(&c.detail);
        e.hold_ms = c.hold_ms;
    }
    state.entries.retain(|k, e| {
        if present.contains(k) {
            return true;
        }
        if e.notified_ms.is_some() || e.attempted_ms.is_some() {
            e.resolved = true;
            true
        } else {
            false
        }
    });
}

/// The posts due now, each with its [`event_id`] under the state's rule set.
pub fn due(state: &AlertState, now_ms: u64, renotify_ms: u64) -> Vec<Event> {
    let mut out = Vec::new();
    for e in state.entries.values() {
        let kind = match (e.resolved, e.notified_ms) {
            (true, _) => Some(EventKind::Resolved),
            (false, None) if now_ms.saturating_sub(e.first_seen_ms) >= e.hold_ms => {
                Some(EventKind::Firing)
            }
            (false, Some(at)) if now_ms.saturating_sub(at) >= renotify_ms => {
                Some(EventKind::Reminder)
            }
            _ => None,
        };
        if let Some(kind) = kind {
            out.push(Event {
                id: event_id(
                    &state.rules,
                    kind,
                    &e.rule,
                    &e.subject,
                    e.first_seen_ms,
                    e.notified_ms,
                ),
                state: kind,
                rule: e.rule.clone(),
                subject: e.subject.clone(),
                detail: e.detail.clone(),
                since_ms: e.first_seen_ms,
                notified_ms: e.notified_ms,
            });
        }
    }
    out
}

/// Marks the posts among `events` as attempted, before they are sent, for a
/// receiver that keeps a ledger of what it sent (the alerts relay): a firing
/// so that its entry resolves with a resolution post when it clears, and a
/// resolution so that a recurrence starts a new episode (see the module
/// note). Call it before posting, and save the state with it.
pub fn attempt(state: &mut AlertState, events: &[Event], now_ms: u64) {
    for ev in events {
        let Some(e) = state.entries.get_mut(&key(&ev.rule, &ev.subject)) else {
            continue;
        };
        match ev.state {
            EventKind::Firing if e.notified_ms.is_none() => {
                e.attempted_ms.get_or_insert(now_ms);
            }
            EventKind::Resolved if e.resolved => {
                e.resolution_attempted_ms.get_or_insert(now_ms);
            }
            _ => {}
        }
    }
}

/// Marks only the resolution posts among `events` as attempted: for a
/// receiver without a ledger (a chat webhook), which could not tell whether
/// an unconfirmed resolution's firing was ever posted, so firings are not
/// marked and an unacknowledged firing that clears is dropped.
pub fn attempt_resolutions(state: &mut AlertState, events: &[Event], now_ms: u64) {
    let resolutions: Vec<Event> = events
        .iter()
        .filter(|e| e.state == EventKind::Resolved)
        .cloned()
        .collect();
    attempt(state, &resolutions, now_ms);
}

/// Marks `events` as posted.
pub fn acknowledge(state: &mut AlertState, events: &[Event], now_ms: u64) {
    for ev in events {
        let k = key(&ev.rule, &ev.subject);
        match ev.state {
            EventKind::Resolved => {
                state.entries.remove(&k);
            }
            EventKind::Firing | EventKind::Reminder => {
                if let Some(e) = state.entries.get_mut(&k) {
                    e.notified_ms = Some(now_ms);
                    e.attempted_ms = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "test.rules/v1";

    #[test]
    fn holds_reminders_resolutions_and_retries() {
        let mut st = AlertState::default();
        let held = Condition {
            rule: "status_unreachable",
            subject: "eval".into(),
            detail: "d".into(),
            hold_ms: 120_000,
        };
        let now = 1_000_000;
        observe(&mut st, R, std::slice::from_ref(&held), now);
        assert_eq!(st.rules, R);
        assert!(due(&st, now, 3_600_000).is_empty(), "inside its hold");
        // A failed post leaves the event due, with the same id.
        observe(&mut st, R, std::slice::from_ref(&held), now + 120_000);
        let first = due(&st, now + 120_000, 3_600_000);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].state, EventKind::Firing);
        observe(&mut st, R, std::slice::from_ref(&held), now + 180_000);
        let ev = due(&st, now + 180_000, 3_600_000);
        assert_eq!(ev, first, "a retry repeats the event and its id");
        acknowledge(&mut st, &ev, now + 180_000);
        // A duplicate run posts nothing.
        observe(&mut st, R, std::slice::from_ref(&held), now + 181_000);
        assert!(due(&st, now + 181_000, 3_600_000).is_empty());
        // A reminder after the renotify interval, with a new id.
        let at = now + 180_000 + 3_600_000;
        observe(&mut st, R, std::slice::from_ref(&held), at);
        let ev = due(&st, at, 3_600_000);
        assert_eq!(ev[0].state, EventKind::Reminder);
        assert_ne!(ev[0].id, first[0].id);
        acknowledge(&mut st, &ev, at);
        // Cleared: a resolution, then nothing.
        observe(&mut st, R, &[], now + 4_000_000);
        let ev = due(&st, now + 4_000_000, 3_600_000);
        assert_eq!(ev[0].state, EventKind::Resolved);
        acknowledge(&mut st, &ev, now + 4_000_000);
        assert!(st.entries.is_empty());
        // A condition that clears inside its hold never posts.
        observe(&mut st, R, std::slice::from_ref(&held), now);
        observe(&mut st, R, &[], now + 60_000);
        assert!(st.entries.is_empty());
    }

    /// A firing post that was attempted but never acknowledged (its answer
    /// was lost) is not dropped when the condition clears: an unconfirmed
    /// resolution is due, so the relay can resolve a firing it did send.
    #[test]
    fn attempted_firings_resolve_when_they_clear() {
        let mut st = AlertState::default();
        let c = Condition {
            rule: "commit_gap",
            subject: "coordinator".into(),
            detail: "d".into(),
            hold_ms: 0,
        };
        let now = 1_000_000;
        observe(&mut st, R, std::slice::from_ref(&c), now);
        let firing = due(&st, now, 3_600_000);
        attempt(&mut st, &firing, now);
        // Retried while it lasts: the same id, the first attempt kept.
        observe(&mut st, R, std::slice::from_ref(&c), now + 60_000);
        let again = due(&st, now + 60_000, 3_600_000);
        assert_eq!(again, firing);
        attempt(&mut st, &again, now + 60_000);
        assert_eq!(
            st.get("commit_gap", "coordinator").unwrap().attempted_ms,
            Some(now)
        );
        // It clears before any answer acknowledged it.
        observe(&mut st, R, &[], now + 120_000);
        let ev = due(&st, now + 120_000, 3_600_000);
        assert_eq!(ev.len(), 1);
        assert!(ev[0].unconfirmed());
        assert_eq!(ev[0].firing_id(R), firing[0].id);
        acknowledge(&mut st, &ev, now + 120_000);
        assert!(st.entries.is_empty());
        // An acknowledged firing clears the attempt: a normal resolution.
        observe(&mut st, R, std::slice::from_ref(&c), now);
        let f = due(&st, now, 3_600_000);
        attempt(&mut st, &f, now);
        acknowledge(&mut st, &f, now + 1);
        assert_eq!(
            st.get("commit_gap", "coordinator").unwrap().attempted_ms,
            None
        );
        observe(&mut st, R, &[], now + 2);
        let z = due(&st, now + 2, 3_600_000);
        assert_eq!(z[0].notified_ms, Some(now + 1));
        assert!(!z[0].unconfirmed());
    }

    /// A resolution whose answer was lost (it may be in the mailbox) and
    /// whose condition comes back starts a new episode: a new firing, with a
    /// new id, after its hold, not one reminder a renotify interval later.
    /// A resolution never attempted still revives the old episode.
    #[test]
    fn a_recurrence_after_an_attempted_resolution_fires_again() {
        let renotify = 6 * 3_600_000;
        let c = Condition {
            rule: "spend_tracker_stale",
            subject: "tracker".into(),
            detail: "d".into(),
            hold_ms: 0,
        };
        let now = 1_000_000;
        let mut st = AlertState::default();
        observe(&mut st, R, std::slice::from_ref(&c), now);
        let firing = due(&st, now, renotify);
        attempt(&mut st, &firing, now);
        acknowledge(&mut st, &firing, now);
        // It clears; the resolution is attempted, and its answer is lost.
        observe(&mut st, R, &[], now + 60_000);
        let resolution = due(&st, now + 60_000, renotify);
        assert_eq!(resolution[0].state, EventKind::Resolved);
        attempt(&mut st, &resolution, now + 60_000);
        // It comes back: a new episode that fires at once (hold 0).
        observe(&mut st, R, std::slice::from_ref(&c), now + 120_000);
        let again = due(&st, now + 120_000, renotify);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].state, EventKind::Firing);
        assert_eq!(again[0].since_ms, now + 120_000);
        assert_ne!(again[0].id, firing[0].id);
        let e = st.get("spend_tracker_stale", "tracker").unwrap();
        assert_eq!((e.attempted_ms, e.resolution_attempted_ms), (None, None));
        // A resolution that was never attempted (no post this run): the
        // old episode revives, and only a reminder follows.
        let mut st = AlertState::default();
        observe(&mut st, R, std::slice::from_ref(&c), now);
        let firing = due(&st, now, renotify);
        acknowledge(&mut st, &firing, now);
        observe(&mut st, R, &[], now + 60_000);
        observe(&mut st, R, std::slice::from_ref(&c), now + 120_000);
        assert!(due(&st, now + 120_000, renotify).is_empty());
        assert_eq!(
            due(&st, now + renotify, renotify)[0].state,
            EventKind::Reminder
        );
        // A plain receiver's marks: resolutions only.
        let mut st = AlertState::default();
        observe(&mut st, R, std::slice::from_ref(&c), now);
        let firing = due(&st, now, renotify);
        attempt_resolutions(&mut st, &firing, now);
        assert_eq!(
            st.get("spend_tracker_stale", "tracker")
                .unwrap()
                .attempted_ms,
            None
        );
        acknowledge(&mut st, &firing, now);
        observe(&mut st, R, &[], now + 60_000);
        let z = due(&st, now + 60_000, renotify);
        attempt_resolutions(&mut st, &z, now + 60_000);
        assert_eq!(
            st.get("spend_tracker_stale", "tracker")
                .unwrap()
                .resolution_attempted_ms,
            Some(now + 60_000)
        );
    }

    /// The serialized shape is the publisher's stored `alerts/v2` state, so a
    /// redeployed publisher reads the state it saved before.
    #[test]
    fn stored_shape_is_unchanged() {
        let stored = r#"{"rules":"slop_ninja.publisher.alerts/v2","entries":{"commit_gap/coordinator":
            {"rule":"commit_gap","subject":"coordinator","detail":"d","hold_ms":0,
             "first_seen_ms":5,"notified_ms":6,"resolved":false}}}"#;
        let st: AlertState = serde_json::from_str(stored).unwrap();
        let e = st.get("commit_gap", "coordinator").unwrap();
        assert_eq!((e.first_seen_ms, e.notified_ms), (5, Some(6)));
        assert_eq!(e.attempted_ms, None);
        let bytes = serde_json::to_vec(&st).unwrap();
        // An entry with no attempt serializes in the stored shape exactly.
        assert!(!String::from_utf8_lossy(&bytes).contains("attempted_ms"));
        assert_eq!(e.resolution_attempted_ms, None);
        let back: AlertState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, st);
    }
}

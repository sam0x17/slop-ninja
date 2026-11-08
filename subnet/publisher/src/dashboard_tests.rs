//! The dashboard documents against the `dashboard/v1` contract: shapes
//! round-trip through the website's own types, the website's fixtures parse
//! into the publisher's, work progress is a grid percent read at tempo
//! boundaries, nothing outside the allow-list or over a bound is written,
//! and no private status field reaches a document.

use super::*;
use crate::index::{ActiveTarget, Publication, TrustRef};
use crate::status::tests::{SECRET_MARKERS, eval_status, worker_status};
use crate::status::{parse_eval, parse_worker};
use crate::store::{MemStore, PublicStore, block_on};
use crate::summary::{self, ModelRef, References};
use serde_json::json;
use slop_ninja_protocol::AccountId;

const TEMPO: u64 = 360;
const B0: u64 = 8_000_000;
const T0_MS: u64 = 1_790_000_000_000;

fn block_of(epoch: u64) -> u64 {
    B0 + epoch * TEMPO
}

fn time_of(epoch: u64) -> u64 {
    T0_MS + epoch * TEMPO * 12_000
}

fn chain() -> ChainBinding {
    ChainBinding {
        genesis: slop_ninja_protocol::TESTNET_GENESIS,
        netuid: 601,
        mechanism: 0,
    }
}

fn settings(c: &ChainBinding) -> Settings<'_> {
    Settings {
        chain: c,
        tempo_blocks: TEMPO,
        block_secs: 12,
        silent_ms: 420_000,
        operator_notice: false,
        record_prefix: Some("testnet-r0-"),
        publish_from_epoch: Some(76),
        roster: RosterLevel::Closed,
    }
}

/// `/eval/status` at epoch `x` with cohort 7 opened at epoch 100 (template
/// 22/16/2, 40 credited tempos at its opening, every later boundary
/// credited), its live engine phase and its phase log, plus the private
/// fields a pass-through would leak. `extra` is merged into cohort 7.
fn eval_json(x: u64, phase: &str, log: &[(&str, u64)], extra: Value) -> Value {
    let log: Vec<Value> = log
        .iter()
        .map(|(p, e)| {
            json!({"phase": p, "at_ms": time_of(*e) + 95_000, "confirmed": block_of(*e) + 3, "epoch": e,
                   "note": "PRIVATE-DOCUMENT-TEXT fault-detail-hash"})
        })
        .collect();
    let mut c7 = json!({
        "cohort": 7, "label": "testnet-r0-c7", "phase": phase, "fault": null, "opened_epoch": 100,
        "opened_block": block_of(100), "opened_active": 40, "k_tempos": 22, "admission_tempos": 16,
        "transition_tempos": 2, "load": false, "roster_cap": 40,
        "progress": {"generation": {"ready": 300, "total": 1000}, "scoring": {"done": 450, "total": 1000}},
        "generation": [{"model": "@cf/x", "cost_microusd": 123, "prompt_tokens": 9}],
        "budget_microusd": 99, "rejects": {"PRIVATE-DOCUMENT-TEXT": 1},
        "docs": [{"kind": "mixed", "status": "ready", "count": 3}],
        "jobs": [{"job": 0, "chunks": 10, "done_chunks": 4, "call_ms": 77}],
        "step_error": {"code": "fault-detail-hash", "since_epoch": 99, "count": 3},
        "log": log,
    });
    if let (Some(m), Value::Object(e)) = (c7.as_object_mut(), extra) {
        m.extend(e);
    }
    let mut v = eval_status("admission");
    v["observed"] = json!({"confirmed": {"number": block_of(x) + 160, "hash": "0xab"},
        "epoch": {"index": x, "block": {"number": block_of(x) + 150, "hash": "0xcd"}, "last_epoch_block": block_of(x)},
        "active_tempos": 40 + x - 100, "epoch_live": true});
    v["cohorts"] = json!([
        {"cohort": 6, "label": "testnet-r0-c6", "phase": "closed", "opened_epoch": 78, "opened_active": 18,
         "k_tempos": 22, "admission_tempos": 16, "transition_tempos": 2, "load": false,
         "reward_v1": {"published_epoch": 99, "probe_build": false}, "window_closed_epoch": 94,
         "log": [
            {"phase": "admission", "at_ms": time_of(78) + 60_000, "epoch": 78, "note": "PRIVATE-DOCUMENT-TEXT"},
            {"phase": "generating", "at_ms": time_of(94) + 60_000, "epoch": 94},
            {"phase": "sealing", "at_ms": time_of(95) + 60_000, "epoch": 95},
            {"phase": "scoring", "at_ms": time_of(95) + 70_000, "epoch": 95},
            {"phase": "packaging", "at_ms": time_of(97) + 60_000, "epoch": 97},
            {"phase": "transition", "at_ms": time_of(98) + 61_000, "epoch": 98},
            {"phase": "closed", "at_ms": time_of(100) + 62_000, "epoch": 100}]},
        c7,
        {"cohort": 8, "label": "testnet-r0-c8", "phase": "queued", "k_tempos": 22, "admission_tempos": 16,
         "transition_tempos": 2, "load": false},
        {"cohort": 900, "label": "load-x", "phase": "scoring", "load": true},
        {"cohort": 3, "label": "testnet-rv1-c3", "phase": "admission", "opened_epoch": 90, "load": false,
         "log": [{"phase": "admission", "epoch": 90}]}
    ]);
    v["reward"]["references"]["k"] = json!(1);
    v["calendar"] = json!({
        "current": {"cohort": 7, "window_open": phase == "admission", "window_closing": false,
                    "freeze_wait": "PRIVATE-DOCUMENT-TEXT"},
        "next": {"cohort": 8, "earliest_open_epoch": 121, "admission_tempos": 16},
        "template": {"sha256": "fault-detail-hash", "paused": false, "last_error": "PRIVATE-DOCUMENT-TEXT"}});
    v["admission"] = json!({"cohorts": [{"cohort": 7, "admitted": 12, "sessions": [{"state": "verified", "count": 2}]}]});
    v
}

fn observed(eval: Value) -> Observed {
    Observed {
        coordinator: Some(parse_worker(&worker_status("primary", false))),
        eval_worker: Some(parse_worker(&worker_status("primary", false))),
        eval: Some(parse_eval(&eval)),
    }
}

/// The memo a run left at the boundary before `x`: the next observation is
/// in sequence and reads the boundary-latched values.
fn saved_at(x: u64) -> Memo {
    Memo {
        epoch: Some(x - 1),
        ..Default::default()
    }
}

/// A state save that succeeds.
async fn saved(_: &Memo) -> bool {
    true
}

/// The memo rolled to the status's boundary, as a run leaves it.
fn rolled(o: &Observed, memo: &mut Memo, now_ms: u64) {
    let c = chain();
    roll(memo, &settings(&c), o.eval.as_ref().unwrap(), now_ms);
}

/// `now.json` written within the tempo of the status's boundary (an hour
/// after it), so every boundary time it carries is no later than the
/// document.
fn now_of(o: &Observed, memo: &Memo) -> NowDoc {
    let c = chain();
    let x = o.eval.as_ref().and_then(|e| e.epoch).unwrap_or(110);
    now_doc(&settings(&c), o, None, None, memo, time_of(x) + 3_600_000)
}

fn value(v: &impl Serialize) -> Value {
    serde_json::to_value(v).unwrap()
}

/// The document round-trips through the website's types unchanged: no field
/// the website would drop, none it lacks.
fn round_trips<T: Serialize + for<'d> Deserialize<'d>>(v: &Value) {
    // The website skips some absent values when it serializes: a `null` and
    // a missing key read the same on both sides.
    fn strip(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(m.iter().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.clone(), strip(x))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(strip).collect()),
            _ => v.clone(),
        }
    }
    let parsed: T = serde_json::from_value(v.clone()).expect("the website's types parse it");
    assert_eq!(strip(&value(&parsed)), strip(v), "round trip through the website's model");
}

fn checked(doc: &Doc, v: &impl Serialize) -> Value {
    let bytes = serde_json::to_vec(v).unwrap();
    check(&doc.key(), &bytes).unwrap_or_else(|e| panic!("{e}"));
    value(v)
}

const LOG_SCORING: &[(&str, u64)] = &[
    ("admission", 100),
    ("generating", 116),
    ("generating", 116),
    ("sealing", 117),
    ("scoring", 117),
];

#[test]
fn admission_now_states_credited_tempos_and_lower_bounds() {
    let o = observed(eval_json(110, "admission", &[("admission", 100)], json!({})));
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(110));
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    let c = &d.cohort;
    assert_eq!(c.stage, "admission_open");
    assert_eq!((c.cohort, c.label.as_deref()), (Some(7), Some("testnet-r0-c7")));
    assert_eq!(
        c.tempos,
        Some(Tempos {
            credited: Some(10),
            required: Some(16)
        })
    );
    assert_eq!(c.expected.stage_end_epoch, Some(116));
    assert_eq!(c.expected.stage_end_block, Some(block_of(116)));
    assert_eq!(c.expected.close_epoch, Some(122));
    assert_eq!(c.expected.packaging_deadline_epoch, None);
    let plan = c.plan.as_ref().unwrap();
    let earliest: Vec<Option<u64>> = plan.stages.iter().map(|s| s.earliest_epoch).collect();
    assert_eq!(
        earliest,
        [None, Some(116), Some(116), Some(116), Some(116), Some(116), Some(116), Some(122)]
    );
    for s in &plan.stages {
        assert_eq!(s.earliest_block, s.earliest_epoch.map(block_of), "{}", s.stage);
    }
    assert_eq!(plan.template.work_window_tempos, Some(4));
    assert_eq!(
        c.progress.admission,
        Some(Admission {
            admitted: Some(12),
            roster_cap: Some(40)
        })
    );
    assert_eq!(c.progress.work, None);
    assert!(!c.results);
    assert_eq!(c.phases.len(), 1);
    assert_eq!(c.phases[0].epoch, Some(100));
    assert_eq!(c.phases[0].at, Some(minute(time_of(100) + 95_000)));
    assert_eq!(d.notice, ["testnet", "burn"]);
    assert_eq!(d.pause, Pause::default());
    assert_eq!(d.sources_ok, Some(true));
    assert_eq!(d.clock.boundary_block, Some(block_of(110)));
    assert_eq!(d.clock.tempo_blocks, Some(TEMPO));
    // The next cohort opens at the boundary that can close this one.
    let next = d.next.as_ref().unwrap();
    assert_eq!((next.cohort, next.earliest_open_epoch), (Some(8), Some(122)));
    assert_eq!(next.earliest_open_block, Some(block_of(122)));
    // The rehearsal cohort outside the record never shows.
    assert!(!serde_json::to_string(&d).unwrap().contains("testnet-rv1"));
}

#[test]
fn a_credited_cutoff_from_the_calendar_wins_and_closing_is_a_step() {
    let mut e = eval_json(116, "admission", &[("admission", 100)], json!({}));
    e["calendar"]["current"]["window_open"] = json!(false);
    e["calendar"]["current"]["window_closing"] = json!(true);
    e["calendar"]["current"]["window_closed_epoch"] = json!(116);
    let o = observed(e);
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(116));
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    assert_eq!(d.cohort.stage, "admission_closing");
    assert_eq!(d.cohort.tempos, None, "a step carries no tempos");
    assert_eq!(d.cohort.expected.stage_end_epoch, Some(117));
    assert_eq!(d.cohort.phases.last().unwrap().epoch, Some(116));
    let mut e = eval_json(110, "admission", &[("admission", 100)], json!({}));
    e["calendar"]["current"]["earliest_cutoff_epoch"] = json!(119);
    e["calendar"]["current"]["earliest_close_epoch_credited"] = json!(125);
    let o = observed(e);
    let d = now_of(&o, &memo);
    assert_eq!(d.cohort.expected.stage_end_epoch, Some(119));
    assert_eq!(d.cohort.expected.close_epoch, Some(125));
    assert_eq!(d.cohort.plan.unwrap().stages[7].earliest_epoch, Some(125));
}

/// A change the engine commits with work is seen at the next boundary; the
/// work percent is read at a boundary and kept until the next one.
#[test]
fn work_changes_are_seen_at_the_next_boundary_and_the_percent_is_latched() {
    // A fresh memo (a first deploy, a lost state) reads nothing mid-tempo.
    let o = observed(eval_json(117, "scoring", LOG_SCORING, json!({})));
    let mut fresh = Memo::default();
    rolled(&o, &mut fresh, time_of(117) + 20 * 60_000);
    assert_eq!(now_of(&o, &fresh).cohort.progress.work, None, "not read mid-tempo");
    let mut memo = saved_at(117);
    // Epoch 117: building started in tempo 116 (seen at 117); sealing and
    // scoring started in tempo 117 and are not seen yet. The engine already
    // left building, so its percent reads 90, never 100.
    let o = observed(eval_json(117, "scoring", LOG_SCORING, json!({})));
    rolled(&o, &mut memo, time_of(117));
    let d = now_of(&o, &memo);
    checked(&Doc::Now("a"), &d);
    assert_eq!(d.cohort.stage, "generating");
    let stages: Vec<(&str, Option<u64>)> = d.cohort.phases.iter().map(|p| (p.stage.as_str(), p.epoch)).collect();
    assert_eq!(stages, [("admission_open", Some(100)), ("generating", Some(117))]);
    assert_eq!(
        d.cohort.progress.work,
        Some(WorkProgress {
            stage: "generating".into(),
            pct: 90,
            as_of_epoch: Some(117)
        })
    );
    assert_eq!(d.cohort.expected.packaging_deadline_epoch, Some(119));
    // A work stage seen at a boundary the publisher observed in sequence
    // carries that boundary's first observed minute.
    assert_eq!(d.cohort.phases[1].at, Some(minute(time_of(117))));
    let mut m2 = Memo {
        epoch: Some(115),
        ..Default::default()
    };
    rolled(&o, &mut m2, time_of(117) + 30_000);
    let d2 = now_of(&o, &m2);
    assert_eq!(d2.cohort.phases[1].at, None, "a boundary not observed in sequence stays undated");
    assert_eq!(d2.cohort.progress.work, None, "nor is its percent read");
    // Epoch 118: scoring is seen, at 450 of 1000 chunks: 40.
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    rolled(&o, &mut memo, time_of(118));
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    assert_eq!(d.cohort.stage, "scoring");
    assert_eq!(d.cohort.progress.work.as_ref().map(|w| w.pct), Some(40));
    assert_eq!(d.cohort.tempos, None);
    // More chunks finish within the same tempo: nothing moves.
    let later = json!({"progress": {"generation": {"ready": 1000, "total": 1000}, "scoring": {"done": 790, "total": 1000}}});
    let o = observed(eval_json(118, "scoring", LOG_SCORING, later.clone()));
    rolled(&o, &mut memo, time_of(118) + 40 * 60_000);
    let d = now_of(&o, &memo);
    assert_eq!(d.cohort.progress.work.as_ref().map(|w| w.pct), Some(40));
    // The next boundary reads 70 (790 floored to the grid).
    let o = observed(eval_json(119, "scoring", LOG_SCORING, later));
    rolled(&o, &mut memo, time_of(119));
    let d = now_of(&o, &memo);
    assert_eq!(d.cohort.progress.work.as_ref().map(|w| (w.pct, w.as_of_epoch)), Some((70, Some(119))));
    // Every chunk done: still 90 at most.
    let done = json!({"progress": {"scoring": {"done": 1000, "total": 1000}}});
    let o = observed(eval_json(120, "scoring", LOG_SCORING, done));
    rolled(&o, &mut memo, time_of(120));
    assert_eq!(now_of(&o, &memo).cohort.progress.work.map(|w| w.pct), Some(90));
    // No counts from an eval Worker without E1: no percent at all.
    let mut e = eval_json(121, "scoring", LOG_SCORING, json!({}));
    e["cohorts"][1].as_object_mut().unwrap().remove("progress");
    let o = observed(e);
    rolled(&o, &mut memo, time_of(121));
    assert_eq!(now_of(&o, &memo).cohort.progress.work, None);
}

#[test]
fn transition_counts_its_credited_tempos_to_the_close() {
    let mut log = LOG_SCORING.to_vec();
    log.extend([("packaging", 119), ("transition", 121)]);
    let o = observed(eval_json(121, "transition", &log, json!({"published_active": 61, "reward_v1": {"published_epoch": 121}})));
    let mut memo = saved_at(121);
    rolled(&o, &mut memo, time_of(121));
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    assert_eq!(d.cohort.stage, "transition");
    assert_eq!(
        d.cohort.tempos,
        Some(Tempos {
            credited: Some(0),
            required: Some(2)
        })
    );
    assert_eq!(d.cohort.expected.close_epoch, Some(123));
    assert_eq!(d.cohort.expected.stage_end_epoch, Some(123));
    let p = d.cohort.plan.unwrap();
    assert_eq!(p.stages[7].earliest_epoch, Some(123));
    assert!(p.stages[..7].iter().all(|s| s.earliest_epoch.is_none()));
    assert!(!d.cohort.results, "no results document before the cohort is listed");
    assert_eq!(d.cohort.progress, Progress::default());
}

#[test]
fn pauses_come_from_the_closed_class_list() {
    let c = chain();
    let s = settings(&c);
    // A silent coordinator: its chain observation is older than the bound.
    let mut o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    o.eval_worker.as_mut().unwrap().confirmed_age_ms = Some(900_000);
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(118));
    let d = now_doc(&s, &o, None, None, &memo, time_of(118));
    assert_eq!(d.pause.class.as_deref(), Some("coordinator_silent"));
    assert!(d.pause.paused);
    checked(&Doc::Now("a"), &d);
    // A fault that clears by itself, read at the boundary.
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({"fault": "scorer_outage", "self_clearing": true})));
    let mut fresh = Memo::default();
    rolled(&o, &mut fresh, time_of(118) + 600_000);
    assert_eq!(now_doc(&s, &o, None, None, &fresh, time_of(118)).pause.class, None, "not read mid-tempo");
    let mut memo = saved_at(118);
    rolled(&o, &mut memo, time_of(118));
    let d = now_doc(&s, &o, None, None, &memo, time_of(118));
    assert_eq!(d.pause.class.as_deref(), Some("waiting_on_infrastructure"));
    let text = serde_json::to_string(&d).unwrap();
    assert!(!text.contains("scorer_outage"), "no fault code");
    // ... which a fault raised mid-tempo does not change until the next
    // boundary.
    let o2 = observed(eval_json(118, "scoring", LOG_SCORING, json!({"fault": "x", "self_clearing": false, "operator_clear_needed": true})));
    rolled(&o2, &mut memo, time_of(118) + 600_000);
    assert_eq!(now_doc(&s, &o2, None, None, &memo, time_of(118)).pause.class.as_deref(), Some("waiting_on_infrastructure"));
    let o3 = observed(eval_json(119, "scoring", LOG_SCORING, json!({"fault": "x", "self_clearing": false, "operator_clear_needed": true})));
    rolled(&o3, &mut memo, time_of(119));
    assert_eq!(now_doc(&s, &o3, None, None, &memo, time_of(119)).pause.class.as_deref(), Some("operator_hold"));
    // An observation catching up.
    let mut e = eval_json(118, "scoring", LOG_SCORING, json!({}));
    e["observed"]["epoch_live"] = json!(false);
    let o = observed(e);
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(118));
    assert_eq!(now_doc(&s, &o, None, None, &memo, time_of(118)).pause.class.as_deref(), Some("observation_lag"));
    // Nothing running and the calendar paused.
    let mut e = eval_json(130, "closed", &[("admission", 100), ("transition", 121), ("closed", 123)], json!({}));
    e["cohorts"][2]["phase"] = json!("closed");
    e["calendar"]["template"]["paused"] = json!(true);
    let o = observed(e);
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(130));
    let d = now_doc(&s, &o, None, None, &memo, time_of(130));
    assert_eq!(d.cohort.stage, "idle");
    assert_eq!(d.pause.class.as_deref(), Some("calendar_paused"));
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
}

#[test]
fn queued_idle_and_unreachable_states() {
    let c = chain();
    let s = settings(&c);
    // Cohort 7 closed at 123; cohort 8 queued.
    let o = observed(eval_json(123, "closed", &[("admission", 100), ("transition", 121), ("closed", 123)], json!({})));
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(123));
    let d = now_doc(&s, &o, None, None, &memo, time_of(123));
    assert_eq!(d.cohort.stage, "queued");
    assert_eq!(d.cohort.cohort, Some(8));
    assert_eq!(d.cohort.expected.stage_end_epoch, Some(124));
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    // No eval status: unknown, sources not OK, no pause, no countdown.
    let o = Observed {
        eval: None,
        ..observed(eval_json(118, "scoring", LOG_SCORING, json!({})))
    };
    let d = now_doc(&s, &o, None, None, &memo, time_of(123));
    assert_eq!(d.cohort.stage, "unknown");
    assert_eq!(d.sources_ok, Some(false));
    assert_eq!(d.pause, Pause::default());
    assert_eq!(d.cohort.expected, Expected::default());
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
}

#[test]
fn off_grid_percents_and_counts_are_refused() {
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let mut memo = saved_at(118);
    rolled(&o, &mut memo, time_of(118));
    let good = checked(&Doc::Now("a"), &now_of(&o, &memo));
    let key = Doc::Now("a").key();
    let refused = |mutate: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        mutate(&mut v);
        check(&key, &serde_json::to_vec(&v).unwrap()).is_err()
    };
    for pct in [json!(35), json!(100), json!(-10), json!("40"), json!(12.5), json!(null)] {
        assert!(refused(&|v| v["cohort"]["progress"]["work"]["pct"] = pct.clone()), "{pct}");
    }
    assert!(refused(&|v| v["cohort"]["progress"]["work"]["done"] = json!(450)));
    assert!(refused(&|v| v["cohort"]["progress"]["work"]["total"] = json!(1000)));
    assert!(refused(&|v| v["cohort"]["progress"]["done"] = json!(450)));
    assert!(refused(&|v| v["cohort"]["progress"]["total"] = json!(1000)));
    assert!(refused(&|v| v["cohort"]["progress"]["scoring"] = json!("underway")));
    assert!(refused(&|v| v["cohort"]["progress"]["generation"] = json!({"ready": 1})));
    // A percent for a step or another stage.
    assert!(refused(&|v| {
        v["cohort"]["stage"] = json!("sealing");
        v["cohort"]["progress"]["work"]["stage"] = json!("sealing");
    }));
    assert!(refused(&|v| v["cohort"]["progress"]["work"]["stage"] = json!("generating")));
    assert!(refused(&|v| v["cohort"]["stage"] = json!("evaluating_hidden_work")));
    assert!(refused(&|v| v["pause"]["class"] = json!("fault-detail-hash")));
    assert!(refused(&|v| v["sample"] = json!(true)));
    assert!(refused(&|v| v["schema"] = json!("slop_ninja.dashboard.cohort/v1")));
    // The unchanged document passes.
    assert!(!refused(&|_| {}));
}

fn hotkey(i: u8) -> String {
    crate::ss58::encode(&AccountId([i; 32]))
}

fn index_with(entries: &[(u64, &str, Option<u8>, u64)]) -> CohortIndex {
    let c = chain();
    let mut idx = CohortIndex::empty(&c);
    idx.trust = TrustRef {
        path: "v1/trust/testnet-trust.json".into(),
        sha256: "00".repeat(32),
    };
    idx.active_target = ActiveTarget {
        digest: "11".repeat(32),
        source: "launch_burn".into(),
        cohort: None,
    };
    idx.record_prefix = Some("testnet-r0-".into());
    for &(cohort, label, promoted, k) in entries {
        idx.cohorts.push(CohortEntry {
            cohort,
            label: label.into(),
            path: format!("v1/cohorts/{label}"),
            opened_epoch: 78,
            closed_epoch: 100,
            promoted: promoted.map(hotkey),
            target: None,
            reference_index: k,
            manifest: "22".repeat(32),
            summary: "33".repeat(32),
            entrants: 4,
            artifacts_published: 1,
            artifacts_withheld: 1,
            artifacts_pending: 2,
            best_q: None,
            quantization: None,
        });
    }
    idx
}

fn summary_for(label: &str, cohort: u64, n: usize, promoted: Option<u8>) -> CohortSummary {
    let c = chain();
    let model = |b: u8| ModelRef {
        content: format!("{b:02x}").repeat(32),
        artifact: format!("{b:02x}").repeat(32),
        q: Some("0.84".into()),
    };
    CohortSummary {
        schema: summary::SUMMARY_SCHEMA.into(),
        network: "testnet".into(),
        chain: slop_ninja_admission_wire::json::Chain::from_binding(&c),
        cohort,
        label: label.into(),
        opened_epoch: 78,
        closed_epoch: 100,
        quantization: Some(summary::Quantization {
            rule: "quant-v1".into(),
            width: "0.02".into(),
        }),
        references: References {
            index: 1,
            current: model(0xaa),
            predecessor: Some(model(0xbb)),
        },
        quality: summary::Quality {
            cutoff: Some(summary::Cutoff {
                q: "0.86".into(),
                mode: "margin".into(),
            }),
            bands: vec![
                summary::Band {
                    band: 1,
                    lower: "0.90".into(),
                    upper: "0.92".into(),
                },
                summary::Band {
                    band: 2,
                    lower: "0.86".into(),
                    upper: "0.88".into(),
                },
            ],
        },
        promoted: promoted.map(hotkey),
        entrants: (0..n)
            .map(|i| {
                let i8 = (i % 250) as u8 + 1;
                summary::Entrant {
                    uid: i as u16,
                    hotkey: if n > 250 {
                        crate::ss58::encode(&AccountId({
                            let mut a = [0u8; 32];
                            a[..8].copy_from_slice(&(i as u64).to_le_bytes());
                            a
                        }))
                    } else {
                        hotkey(i8)
                    },
                    hotkey_hex: "ab".repeat(32),
                    q: match i % 4 {
                        2 => None,
                        1 => Some("0.86".into()),
                        _ => Some("0.90".into()),
                    },
                    band: match i % 4 {
                        0 => Some(1),
                        1 => Some(2),
                        2 => None,
                        _ => Some(1),
                    },
                    gate: if i % 4 == 2 { "not_executed" } else { "pass" }.into(),
                    copy_gate: Some("pass".into()),
                    cluster: None,
                }
            })
            .collect(),
    }
}

fn artifacts_for(s: &CohortSummary) -> Vec<ArtifactEntry> {
    s.entrants
        .iter()
        .enumerate()
        .map(|(i, e)| ArtifactEntry {
            uid: e.uid,
            hotkey: e.hotkey.clone(),
            sha256: format!("{:02x}", i % 256).repeat(32),
            content: "cc".repeat(32),
            state: if i == 0 { ArtifactState::Published } else { ArtifactState::Pending },
            path: (i == 0).then(|| format!("artifacts/v2/{}/header", format!("{:02x}", i % 256).repeat(32))),
            reason: Some("PRIVATE-DOCUMENT-TEXT".into()),
            hotkey_hex: "ab".repeat(32),
            registration_block: 7,
            commitment: "dd".repeat(32),
            nonce: "ee".repeat(16),
        })
        .collect()
}

#[test]
fn a_closed_cohort_document_carries_quantized_scores_bands_and_gates_but_no_rank() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(5, "testnet-r0-c5", None, 1), (6, "testnet-r0-c6", Some(1), 1)]);
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let rows = listed(&s, &idx, o.eval.as_ref());
    assert_eq!(rows.iter().map(|r| r.row.label.as_str()).collect::<Vec<_>>(), ["testnet-r0-c5", "testnet-r0-c6"]);
    let sm = summary_for("testnet-r0-c6", 6, 4, Some(1));
    let arts = artifacts_for(&sm);
    let d = cohort_doc(&s, &rows, 1, Some(&sm), &arts, 118, time_of(118));
    let v = checked(&Doc::Cohort("a", "testnet-r0-c6".into()), &d);
    round_trips::<web_model::CohortDoc>(&v);
    assert_eq!(d.cohort.state, "closed");
    assert!(d.cohort.leaderboard);
    assert_eq!(d.cohort.promoted.as_ref().unwrap().uid, Some(0));
    let en = d.entrants.as_ref().unwrap();
    assert_eq!(en.len(), 4);
    for (e, s) in en.iter().zip(&sm.entrants) {
        assert_eq!(e.eligible, e.band.is_some());
        // The summary's q, under the summary's quantization block.
        assert_eq!(e.score, s.q);
        assert_eq!(e.row_share_pct, None);
    }
    assert_eq!(
        d.cohort.quantization,
        Some(Quantization { rule: Some("quant-v1".into()), width: Some("0.02".into()) })
    );
    assert_eq!(d.cohort.best_score.as_deref(), Some("0.90"));
    assert_eq!(d.score_better.as_deref(), Some(SCORE_BETTER));
    assert!(!en[2].gates_passed && en[0].gates_passed);
    assert!(en[0].promoted && !en[1].promoted);
    assert_eq!(en[0].artifact.as_ref().unwrap().state, "published");
    assert!(en[0].artifact.as_ref().unwrap().download.as_deref().unwrap().starts_with("artifacts/v2/"));
    assert_eq!(en[1].artifact.as_ref().unwrap().download, None);
    let q = d.quality.as_ref().unwrap();
    assert_eq!(q.cutoff, Some(Cutoff { q: Some("0.86".into()), rule: Some("margin".into()) }));
    // Band 1 is open above; band 2's upper edge is band 1's lower one (the
    // empty grid band between them merges into band 2), and the last band's
    // lower edge is the cutoff.
    let st = |t: &str| Some(t.to_owned());
    assert_eq!(
        q.bands.as_ref().unwrap().iter().map(|b| (b.band, b.lower.clone(), b.upper.clone())).collect::<Vec<_>>(),
        [(1, st("0.90"), None), (2, st("0.86"), st("0.90"))]
    );
    assert_eq!(d.reference.as_ref().map(|r| (r.index, r.label.as_str(), r.score.clone())), Some((1, "R1", st("0.84"))));
    assert_eq!(d.predecessor.as_ref().map(|r| r.label.as_str()), Some("R0"));
    assert_eq!(d.promotion_rule.as_deref(), Some(TESTNET_PROMOTION_RULE));
    assert_eq!((d.previous.as_deref(), d.next.as_deref()), (Some("testnet-r0-c5"), None));
    // Its phases come from the phase log: work changes one boundary late,
    // and only boundary steps carry a time.
    let ph: Vec<(&str, Option<u64>, bool)> = d.phases.iter().map(|p| (p.stage.as_str(), p.epoch, p.at.is_some())).collect();
    assert_eq!(
        ph,
        [
            ("admission_open", Some(78), true),
            ("admission_closing", Some(94), false),
            ("generating", Some(95), false),
            ("sealing", Some(96), false),
            ("scoring", Some(96), false),
            ("packaging", Some(98), false),
            ("transition", Some(99), false),
            ("closed", Some(100), true)
        ]
    );
    let text = serde_json::to_string(&d).unwrap();
    for leak in ["rank", "hotkey_hex", "cluster", "copy_gate", "\"gate\"", "commitment", "nonce", "sha256", "PRIVATE"] {
        assert!(!text.contains(leak), "{leak}");
    }
    // A cohort without its summary: no entrants, no chart, no rule.
    let d = cohort_doc(&s, &rows, 0, None, &[], 118, time_of(118));
    checked(&Doc::Cohort("a", "testnet-r0-c5".into()), &d);
    assert!(d.entrants.is_none() && d.quality.is_none() && !d.cohort.leaderboard);
    // The cohort list.
    let list = cohorts_doc(&s, &idx, &rows, time_of(118));
    let v = checked(&Doc::Cohorts("a"), &list);
    round_trips::<web_model::CohortsDoc>(&v);
    assert_eq!(list.record_prefix.as_deref(), Some("testnet-r0-"));
    assert!(list.cohorts.iter().all(|r| r.best_score.is_none() && r.quantization.is_none() && r.mode.is_none()));
}

#[test]
fn entrants_never_appear_before_results_and_ranks_never() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1)]);
    let rows = listed(&s, &idx, None);
    let sm = summary_for("testnet-r0-c6", 6, 4, None);
    let good = checked(
        &Doc::Cohort("a", "testnet-r0-c6".into()),
        &cohort_doc(&s, &rows, 0, Some(&sm), &[], 0, time_of(118)),
    );
    let key = Doc::Cohort("a", "testnet-r0-c6".into()).key();
    let refused = |mutate: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        mutate(&mut v);
        check(&key, &serde_json::to_vec(&v).unwrap()).is_err()
    };
    assert!(refused(&|v| v["cohort"]["state"] = json!("ended")));
    assert!(refused(&|v| v["entrants"][0]["rank"] = json!(1)));
    assert!(refused(&|v| v["entrants"][0]["raw"] = json!("846878585280111913")));
    assert!(refused(&|v| v["entrants"][0]["assigned"] = json!(1000)));
    assert!(refused(&|v| v["cohort"]["receipts"] = json!(32)));
    assert!(refused(&|v| v["quality"]["cutoff"] = json!({"q": "0.7", "rule": "x", "exact": "0.71234"})));
    assert!(refused(&|v| v["records"] = json!([])));
    assert!(!refused(&|_| {}));
}

#[test]
fn an_ended_cohort_is_listed_once_its_end_is_visible() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1)]);
    let log = [("admission", 100), ("generating", 116), ("abandoned", 117)];
    // Abandoned in tempo 117: not seen before boundary 118.
    let o = observed(eval_json(117, "abandoned", &log, json!({})));
    assert_eq!(listed(&s, &idx, o.eval.as_ref()).len(), 1);
    let mut memo = Memo::default();
    rolled(&o, &mut memo, time_of(117));
    assert_eq!(now_of(&o, &memo).cohort.stage, "generating");
    let o = observed(eval_json(118, "abandoned", &log, json!({})));
    let rows = listed(&s, &idx, o.eval.as_ref());
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[1].row.state.as_str(), rows[1].row.ended_class.clone()), ("ended", None));
    let d = cohort_doc(&s, &rows, 1, None, &[], 118, time_of(118));
    let v = checked(&Doc::Cohort("a", "testnet-r0-c7".into()), &d);
    round_trips::<web_model::CohortDoc>(&v);
    assert!(d.entrants.is_none());
    assert_eq!(d.phases.last().map(|p| p.stage.as_str()), Some("ended"));
    assert_eq!(d.previous.as_deref(), Some("testnet-r0-c6"));
    rolled(&o, &mut memo, time_of(118));
    let n = now_of(&o, &memo);
    assert_eq!(n.cohort.stage, "ended");
    checked(&Doc::Now("a"), &n);
}

/// The coordinator's latest weight commit at or below the document's chain
/// head (epoch and block), with its reveal outcome: `revealed` with its block
/// only when that follows the commit within the head, `missed` for an
/// operation that failed after its commit, else `pending`; public chain facts
/// only (no extrinsic hash, row or weights).
#[test]
fn weights_carry_the_latest_commit_within_the_chain_head() {
    use crate::status::OpView;
    let mut log = LOG_SCORING.to_vec();
    log.extend([("packaging", 119), ("transition", 121)]);
    let mut o = observed(eval_json(121, "transition", &log, json!({"published_active": 61, "reward_v1": {"published_epoch": 121}})));
    let mut memo = saved_at(121);
    rolled(&o, &mut memo, time_of(121));
    let clock = now_of(&o, &memo).clock;
    let head = clock.confirmed_block.expect("a chain head");
    let head_epoch = clock.confirmed_epoch.expect("a chain head epoch");
    let op = |epoch: u64, commit: u64, reveal: Option<u64>| OpView {
        epoch,
        state: if reveal.is_some() { "revealed" } else { "committed" }.into(),
        commit_block: Some(commit),
        commit_index: Some(3),
        reveal_block: reveal,
        ..OpView::default()
    };
    let failed = |epoch: u64, commit: u64, detail: &str| OpView {
        state: "failed".into(),
        detail: Some(detail.into()),
        ..op(epoch, commit, None)
    };
    let set = |o: &mut Observed, ops: Vec<OpView>| {
        o.coordinator.as_mut().unwrap().signing.as_mut().unwrap().ops = ops;
    };
    let w = |epoch: u64, commit: u64, reveal: &str, reveal_block: Option<u64>| {
        Some(Weights { epoch, commit_block: commit, commit_index: Some(3), reveal: reveal.into(), reveal_block })
    };
    // The newest commit past the head waits; the next one is published with its reveal.
    set(&mut o, vec![op(head_epoch, head + 5, None), op(head_epoch - 1, head - 300, Some(head - 10)), op(head_epoch - 2, head - 700, Some(head - 400))]);
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    assert_eq!(d.weights, w(head_epoch - 1, head - 300, REVEAL_REVEALED, Some(head - 10)));
    assert_eq!(
        v["weights"],
        json!({"epoch": head_epoch - 1, "commit_block": head - 300, "commit_index": 3, "reveal": "revealed", "reveal_block": head - 10})
    );
    // An epoch past the head's, though committed within it, waits too: the
    // previous commit is published rather than nothing.
    set(&mut o, vec![op(head_epoch + 1, head - 5, None), op(head_epoch, head - 300, None)]);
    assert_eq!(now_of(&o, &memo).weights, w(head_epoch, head - 300, REVEAL_PENDING, None));
    // Two operations for one epoch: the later commit.
    set(&mut o, vec![op(head_epoch, head - 20, None), failed(head_epoch, head - 300, "reveal_dropped")]);
    assert_eq!(now_of(&o, &memo).weights, w(head_epoch, head - 20, REVEAL_PENDING, None));
    // A failed operation that had committed will never reveal: missed, not pending.
    for detail in ["reveal_missed", "reveal_dropped"] {
        set(&mut o, vec![failed(head_epoch, head - 300, detail), op(head_epoch - 1, head - 700, Some(head - 400))]);
        let d = now_of(&o, &memo);
        assert_eq!(d.weights, w(head_epoch, head - 300, REVEAL_MISSED, None), "{detail}");
        let v = checked(&Doc::Now("a"), &d);
        round_trips::<web_model::NowDoc>(&v);
        assert_eq!(v["weights"]["reveal"], "missed");
    }
    // A reveal past the head, or not after its commit, is pending; a reveal
    // block on an operation not marked revealed is not published.
    set(&mut o, vec![op(head_epoch, head - 300, Some(head + 1))]);
    assert_eq!(now_of(&o, &memo).weights, w(head_epoch, head - 300, REVEAL_PENDING, None));
    set(&mut o, vec![op(head_epoch, head - 300, Some(head - 300))]);
    assert_eq!(now_of(&o, &memo).weights, w(head_epoch, head - 300, REVEAL_PENDING, None));
    set(&mut o, vec![OpView { reveal_block: Some(head - 10), ..op(head_epoch, head - 300, None) }]);
    assert_eq!(now_of(&o, &memo).weights, w(head_epoch, head - 300, REVEAL_PENDING, None));
    // A commit index the chain could not hold is left out, not the commit.
    set(&mut o, vec![OpView { commit_index: Some(u64::from(u32::MAX) + 1), ..op(head_epoch, head - 300, None) }]);
    assert_eq!(now_of(&o, &memo).weights.unwrap().commit_index, None);
    // No commit within the head, or no signing status: no weights.
    set(&mut o, vec![op(head_epoch, head + 5, None)]);
    assert_eq!(now_of(&o, &memo).weights, None);
    o.coordinator.as_mut().unwrap().signing = None;
    let d = now_of(&o, &memo);
    assert_eq!(d.weights, None);
    round_trips::<web_model::NowDoc>(&checked(&Doc::Now("a"), &d));
}

/// `clean_now` holds the website's rule for `weights` whatever the input: a
/// zero commit block or an unknown outcome drops the object; a reveal block
/// rides only with `revealed`, and `revealed` needs one.
#[test]
fn clean_now_holds_the_weights_rule() {
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let mut memo = saved_at(118);
    rolled(&o, &mut memo, time_of(118));
    let base = now_of(&o, &memo);
    let head = base.clock.confirmed_block.expect("a chain head");
    let epoch = base.clock.confirmed_epoch.expect("a chain head epoch");
    let cleaned = |reveal: &str, commit: u64, reveal_block: Option<u64>| {
        let mut d = base.clone();
        d.weights = Some(Weights { epoch, commit_block: commit, commit_index: None, reveal: reveal.into(), reveal_block });
        clean_now(&mut d);
        d.weights.map(|w| (w.reveal, w.reveal_block))
    };
    assert_eq!(cleaned("revealed", head - 9, Some(head - 1)), Some(("revealed".into(), Some(head - 1))));
    assert_eq!(cleaned("revealed", head - 9, None), Some(("pending".into(), None)));
    assert_eq!(cleaned("missed", head - 9, Some(head - 1)), Some(("missed".into(), None)));
    assert_eq!(cleaned("pending", head - 9, Some(head - 1)), Some(("pending".into(), None)));
    assert_eq!(cleaned("later", head - 9, None), None);
    assert_eq!(cleaned("pending", 0, None), None);
}

/// The coordinator's UID map, as of a boundary within the document's head:
/// SS58 hotkeys in UID order, checked by the allow-list and the website's
/// types; a map that is not whole, or past the head, is dropped whole and
/// never refuses the document; a full map fits the `now` bound.
#[test]
fn uids_carry_the_coordinators_map_within_the_head() {
    let hx = |n: u8| hex::encode([n; 32]);
    let ss = |n: u8| crate::ss58::encode(&AccountId([n; 32]));
    let with = |map: Value| {
        let mut e = eval_json(118, "scoring", LOG_SCORING, json!({}));
        e["uid_map"] = map;
        observed(e)
    };
    let good = json!({"rule": crate::status::UID_MAP_RULE, "epoch": 118, "block": block_of(118),
        "rows": [{"uid": 3, "hotkey": hx(1)}, {"uid": 9, "hotkey": hx(2)}]});
    let o = with(good.clone());
    let mut memo = saved_at(118);
    rolled(&o, &mut memo, time_of(118));
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    assert_eq!(
        v["uids"],
        json!({"epoch": 118, "block": block_of(118), "rows": [{"uid": 3, "hotkey": ss(1)}, {"uid": 9, "hotkey": ss(2)}]})
    );
    // Not whole or not this rule: no map, and the document is still written.
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut m = good.clone();
        f(&mut m);
        let d = now_of(&with(m), &memo);
        checked(&Doc::Now("a"), &d);
        d.uids
    };
    assert_eq!(bad(&|m| m["rule"] = json!("slop_ninja.eval.uid_map/v0")), None, "another rule");
    assert_eq!(bad(&|m| m["rows"] = Value::Null), None, "refused by the coordinator");
    assert_eq!(bad(&|m| m["rows"][1]["uid"] = json!(3)), None, "a repeated UID");
    assert_eq!(bad(&|m| m["rows"][1]["uid"] = json!(2)), None, "UIDs out of order");
    assert_eq!(bad(&|m| m["rows"][1]["uid"] = json!(70_000)), None, "a UID outside u16");
    assert_eq!(bad(&|m| m["rows"][1]["hotkey"] = json!(hx(1))), None, "a repeated hotkey");
    assert_eq!(bad(&|m| m["rows"][1]["hotkey"] = json!(hx(0xab).to_uppercase())), None, "not lowercase hex");
    assert_eq!(bad(&|m| m["rows"][1]["hotkey"] = json!("ab")), None, "not 32 bytes");
    assert_eq!(bad(&|m| m["epoch"] = json!(119)), None, "an epoch past the boundary");
    assert_eq!(bad(&|m| m["block"] = json!(block_of(119))), None, "a block past the head");
    let over: Vec<Value> = (0..=contract::uid_map::ROWS as u16)
        .map(|u| json!({"uid": u, "hotkey": hex::encode([&[(u >> 8) as u8, u as u8][..], &[1; 30]].concat())}))
        .collect();
    assert_eq!(bad(&|m| m["rows"] = json!(over)), None, "more rows than the contract allows");
    // A coordinator built before the map: no field, no change.
    let d = now_of(&observed(eval_json(118, "scoring", LOG_SCORING, json!({}))), &memo);
    assert_eq!(d.uids, None);
    round_trips::<web_model::NowDoc>(&checked(&Doc::Now("a"), &d));
    // A full map of the longest rows fits the `now` bound.
    let full: Vec<Value> = (0..contract::uid_map::ROWS as u16)
        .map(|u| json!({"uid": u16::MAX - (contract::uid_map::ROWS as u16 - 1) + u,
            "hotkey": hex::encode([&[(u >> 8) as u8, u as u8][..], &[0xff; 30]].concat())}))
        .collect();
    let mut m = good.clone();
    m["rows"] = json!(full);
    let d = now_of(&with(m), &memo);
    assert_eq!(d.uids.as_ref().map(|u| u.rows.len()), Some(contract::uid_map::ROWS));
    let bytes = serde_json::to_vec(&d).unwrap();
    let map = serde_json::to_vec(&d.uids).unwrap();
    assert!(map.len() <= contract::uid_map::ROWS * contract::uid_map::ROW_BYTES, "{} bytes", map.len());
    assert!(bytes.len() <= contract::bounds::NOW);
    check(&Doc::Now("a").key(), &bytes).unwrap();
}

#[test]
fn no_private_status_field_reaches_a_document() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", Some(1), 1)]);
    let sm = summary_for("testnet-r0-c6", 6, 4, Some(1));
    let arts = artifacts_for(&sm);
    for (x, phase) in [(110, "admission"), (118, "scoring"), (121, "transition")] {
        let mut log = LOG_SCORING.to_vec();
        log.extend([("packaging", 119), ("transition", 121)]);
        let mut e = eval_json(x, phase, &log, json!({"fault": "fault-detail-hash", "fault_cause": "PRIVATE-DOCUMENT-TEXT"}));
        e["ledger"]["standing"]["spent_microusd"] = json!(900);
        let o = observed(e);
        let mut memo = saved_at(x);
        rolled(&o, &mut memo, time_of(x));
        let since = promoted_by(&idx, 2);
        let facts = BestFacts {
            index: 2,
            since,
            summary: Some(&sm),
            artifacts: &arts,
        };
        let rows = listed(&s, &idx, o.eval.as_ref());
        let docs = [
            serde_json::to_string(&now_doc(&s, &o, Some(&idx), Some(&facts), &memo, time_of(x))).unwrap(),
            serde_json::to_string(&index_doc(&s, time_of(x))).unwrap(),
            serde_json::to_string(&cohorts_doc(&s, &idx, &rows, time_of(x))).unwrap(),
            serde_json::to_string(&cohort_doc(&s, &rows, 0, Some(&sm), &arts, x, time_of(x))).unwrap(),
        ];
        for d in &docs {
            for m in SECRET_MARKERS
                .iter()
                .chain(&["note", "at_ms", "fault", "step_error", "microusd", "\"done\"", "\"total\"", "\"ready\"", "freeze_wait", "sessions", "chunks", "testnet-rv1", "load-x", "@cf"])
            {
                assert!(!d.contains(m), "{m} in {d}");
            }
        }
    }
}

#[test]
fn the_best_model_is_the_cohort_that_promoted_the_current_reference() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(5, "testnet-r0-c5", Some(9), 0), (6, "testnet-r0-c6", None, 1)]);
    assert_eq!(promoted_by(&idx, 1).map(|c| c.cohort), Some(5));
    assert_eq!(promoted_by(&idx, 0), None);
    let sm = summary_for("testnet-r0-c5", 5, 12, Some(9));
    let arts = artifacts_for(&sm);
    let facts = BestFacts {
        index: 1,
        since: promoted_by(&idx, 1),
        summary: Some(&sm),
        artifacts: &arts,
    };
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let memo = Memo::default();
    let d = now_doc(&s, &o, Some(&idx), Some(&facts), &memo, time_of(118));
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    let b = d.best.unwrap();
    assert_eq!((b.index, b.label.as_str(), b.since_cohort, b.since_label.as_deref()), (1, "R1", Some(5), Some("testnet-r0-c5")));
    assert_eq!(b.owner.as_ref().map(|o| (o.uid, o.hotkey.clone())), Some((Some(8), hotkey(9))));
    // The promoted entrant's q in the cohort that promoted it, under its grid.
    assert_eq!(
        (b.latest_score.as_deref(), b.latest_score_label.as_deref(), b.licence),
        (Some("0.90"), Some("testnet-r0-c5"), None)
    );
    assert_eq!(b.quantization, Some(Quantization { rule: Some("quant-v1".into()), width: Some("0.02".into()) }));
    assert_eq!(d.latest_cohort.as_deref(), Some("testnet-r0-c6"));
}

#[test]
fn a_full_roster_fits_the_bound_and_a_larger_document_is_refused() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", Some(1), 1)]);
    let rows = listed(&s, &idx, None);
    let sm = summary_for("testnet-r0-c6", 6, slop_ninja_protocol::MAX_ROSTER as usize, None);
    let arts = artifacts_for(&sm);
    let d = cohort_doc(&s, &rows, 0, Some(&sm), &arts, 0, time_of(118));
    let bytes = serde_json::to_vec(&d).unwrap();
    assert!(bytes.len() <= contract::bounds::ROSTER, "{} bytes", bytes.len());
    // Each row stays far inside the bound's 4 KiB a row.
    assert!(bytes.len() / sm.entrants.len() < 4096);
    check(&Doc::Cohort("a", "testnet-r0-c6".into()).key(), &bytes).unwrap();
    // Over a bound: refused before it is parsed.
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let mut padded = serde_json::to_vec(&now_of(&o, &Memo::default())).unwrap();
    padded.resize(contract::bounds::NOW + 1, b' ');
    let err = check(&Doc::Now("a").key(), &padded).unwrap_err();
    assert!(err.contains("exceed the bound"), "{err}");
    // Keys outside the grammar.
    for key in ["v1/status.json", "dashboard/v1/m/a/cohorts/index.json.bak", "dashboard/v1/m/a/months/2026-11.json"] {
        assert!(check(key, b"{}").is_err(), "{key}");
    }
}

/// The website's own fixtures (its `src/sample.rs`, re-encoded by its
/// contract check) parse into the publisher's types with no unknown field
/// and lose nothing on the way back: the two sides name the same fields.
#[test]
fn the_websites_fixtures_parse_into_the_publisher_types() {
    fn same<T: Serialize + for<'d> Deserialize<'d>>(name: &str, text: &str) {
        let v: Value = serde_json::from_str(text).unwrap();
        let parsed: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(value(&parsed), v, "{name}");
    }
    macro_rules! fixture {
        ($t:ty, $($name:literal),+) => {
            $(same::<$t>($name, include_str!(concat!("../tests/fixtures/dashboard-web/", $name, ".json")));)+
        };
    }
    fixture!(
        NowDoc,
        "now-admission",
        "now-closing",
        "now-building",
        "now-scoring",
        "now-transition",
        "now-queued",
        "now-idle",
        "now-paused",
        "now-hold",
        "now-epoch-only"
    );
    fixture!(CohortsDoc, "cohorts-scoring");
    fixture!(IndexDoc, "index-scoring");
    fixture!(
        CohortDoc,
        "cohort-scoring-testnet-c117",
        "cohort-scoring-testnet-c112",
        "cohort-transition-testnet-c118",
        "cohort-no-scores-testnet-c117"
    );
}

#[test]
fn the_index_document_lists_a_live_and_b_c_planned() {
    let c = chain();
    let d = index_doc(&settings(&c), time_of(118));
    let v = checked(&Doc::Index, &d);
    round_trips::<web_model::IndexDoc>(&v);
    let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/dashboard-web/index-scoring.json")).unwrap();
    assert_eq!(v["mechanisms"], fixture["mechanisms"]);
    assert_eq!(d.chain.netuid, Some(601));
}

#[test]
fn a_run_writes_only_dashboard_keys_and_rebuilds_cohort_documents_on_change() {
    let c = chain();
    let dash = MemStore::new();
    let public_bucket = MemStore::new();
    let idx = index_with(&[(5, "testnet-r0-c5", Some(9), 0), (6, "testnet-r0-c6", None, 1)]);
    for (n, label, promoted) in [(5u64, "testnet-r0-c5", Some(9u8)), (6, "testnet-r0-c6", None)] {
        let sm = summary_for(label, n, 4, promoted);
        public_bucket.insert(&format!("v1/cohorts/{label}/summary.json"), serde_json::to_vec(&sm).unwrap());
        let p = Publication {
            schema: crate::index::PUBLICATION_SCHEMA.into(),
            cohort: n,
            label: label.into(),
            publisher: "test".into(),
            summary: "33".repeat(32),
            artifacts: artifacts_for(&sm),
        };
        public_bucket.insert(&crate::layout::publication(label), serde_json::to_vec(&p).unwrap());
    }
    let out = DashboardStore::new(&dash);
    let public = PublicStore::new(&public_bucket);
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let mut memo = saved_at(118);
    let budget = crate::arena::Budget::worker();
    let run = |memo: &mut Memo, o: &Observed, idx: &CohortIndex, in_budget: bool| {
        let inputs = RunInputs {
            settings: settings(&c),
            observed: o,
            index: Some(idx),
            now_ms: time_of(118),
            budget: &budget,
        };
        block_on(publish(&out, &public, &inputs, memo, saved, &|| in_budget))
    };
    let pass = run(&mut memo, &o, &idx, true);
    assert!(pass.refused.is_empty(), "{:?}", pass.refused);
    let keys = dash.keys();
    assert!(keys.iter().all(|k| k.starts_with("dashboard/v1/")), "{keys:?}");
    for k in [
        "dashboard/v1/index.json",
        "dashboard/v1/m/a/now.json",
        "dashboard/v1/m/a/cohorts/index.json",
        "dashboard/v1/m/a/cohorts/testnet-r0-c5.json",
        "dashboard/v1/m/a/cohorts/testnet-r0-c6.json",
        "dashboard/v1/m/a/rosters/testnet-r0-c5.json",
        "dashboard/v1/m/a/rosters/testnet-r0-c6.json",
    ] {
        assert!(keys.contains(&k.to_owned()), "{k} in {keys:?}");
        let bytes = dash.bytes(k).unwrap();
        check(k, &bytes).unwrap();
    }
    let now: NowDoc = serde_json::from_slice(&dash.bytes("dashboard/v1/m/a/now.json").unwrap()).unwrap();
    assert_eq!(now.best.as_ref().map(|b| b.since_cohort), Some(Some(5)));
    assert_eq!(now.cohort.progress.work.as_ref().map(|w| w.pct), Some(40));
    // The same inputs again: the cohort documents are not rewritten.
    let pass = run(&mut memo, &o, &idx, true);
    assert_eq!(pass.written.len(), 3, "{:?}", pass.written);
    // An artifact copy finishing changes the index entry: rebuilt.
    let mut idx2 = idx.clone();
    idx2.cohorts[1].artifacts_pending = 1;
    idx2.cohorts[1].artifacts_published = 2;
    // Each closed cohort's document and its roster (rule v3).
    let pass = run(&mut memo, &o, &idx2, true);
    assert_eq!(pass.written.len(), 7, "{:?}", pass.written);
    // Out of wall time: the rebuild stops and resumes next run.
    idx2.cohorts[1].artifacts_pending = 0;
    let pass = run(&mut memo, &o, &idx2, false);
    assert!(pass.incomplete && pass.written.len() == 3);
    let pass = run(&mut memo, &o, &idx2, true);
    assert_eq!(pass.written.len(), 7);
    // The store refuses anything outside the grammar or the allow-list.
    assert_eq!(block_on(out.put("v1/status.json", b"{}".to_vec())), Err(StoreError::NotAllowed));
    let mut bad: Value = serde_json::from_slice(&dash.bytes("dashboard/v1/m/a/now.json").unwrap()).unwrap();
    bad["cohort"]["progress"]["work"]["pct"] = json!(45);
    assert_eq!(
        block_on(out.put("dashboard/v1/m/a/now.json", serde_json::to_vec(&bad).unwrap())),
        Err(StoreError::NotAllowed)
    );
    // Without an index (phase 2 failed): the registry and now only.
    let dash2 = MemStore::new();
    let out2 = DashboardStore::new(&dash2);
    let inputs = RunInputs {
        settings: settings(&c),
        observed: &o,
        index: None,
        now_ms: time_of(118),
        budget: &budget,
    };
    let pass = block_on(publish(&out2, &public, &inputs, &mut Memo::default(), saved, &|| true));
    assert_eq!(pass.written, ["dashboard/v1/index.json", "dashboard/v1/m/a/now.json"]);
    assert_eq!(budget.held(), 0, "every reservation released");
    // A budget too small for a cohort's records: refused and retried, the
    // rest still written.
    let tiny = crate::arena::Budget::new(1024);
    let inputs = RunInputs {
        settings: settings(&c),
        observed: &o,
        index: Some(&idx),
        now_ms: time_of(118),
        budget: &tiny,
    };
    let mut m = Memo::default();
    let pass = block_on(publish(&out2, &public, &inputs, &mut m, saved, &|| true));
    assert!(pass.refused.iter().any(|(k, _)| k.contains("testnet-r0-c6")), "{:?}", pass.refused);
    assert!(pass.written.contains(&"dashboard/v1/m/a/now.json".to_owned()));
    assert_eq!(m.cohorts_key, None, "retried next run");
}

#[test]
fn the_scan_allows_the_contract_keys_only_where_the_contract_puts_them() {
    assert!(scan(&json!({"cohort": {"results": true, "plan": {"stages": [{"kind": "work"}]}}})).is_ok());
    assert!(scan(&json!({"results": true})).is_err());
    assert!(scan(&json!({"cohort": {"results": "yes"}})).is_err());
    assert!(scan(&json!({"cohort": {"plan": {"stages": [{"kind": "mixed"}]}}})).is_err());
    assert!(scan(&json!({"entrants": [{"kind": "work"}]})).is_err());
    assert!(scan(&json!({"cohort": {"cost_microusd": 1}})).is_err());
    assert!(scan(&json!({"x": "a".repeat(crate::redact::MAX_PUBLIC_STRING + 1)})).is_err());
}

/// The allow-list names exactly the website model's fields: every path a
/// fully populated document has is allowed, and nothing more is.
#[test]
fn the_allow_list_is_exactly_the_contract() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(5, "testnet-r0-c5", Some(9), 0), (6, "testnet-r0-c6", Some(1), 1)]);
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let mut memo = saved_at(118);
    rolled(&o, &mut memo, time_of(118));
    let sm = summary_for("testnet-r0-c6", 6, 4, Some(1));
    let arts = artifacts_for(&sm);
    let facts = BestFacts {
        index: 2,
        since: promoted_by(&idx, 2),
        summary: Some(&sm),
        artifacts: &arts,
    };
    // Fill every optional block so each allowed path occurs.
    let mut now = value(&now_doc(&s, &o, Some(&idx), Some(&facts), &memo, time_of(118)));
    now["cohort"]["progress"]["admission"] = json!({"admitted": 1, "roster_cap": 2});
    now["cohort"]["tempos"] = json!({"credited": 1, "required": 2});
    now["best"]["quantization"] = json!({"rule": "r", "width": "0.01"});
    let point = json!({"epoch": 1, "block": 2, "at": 60, "basis": "observed"});
    now["cohort"]["bounds"]["cutoff"] = point.clone();
    now["weights"] = json!({"epoch": 1, "commit_block": 2, "commit_index": 3, "reveal": "revealed", "reveal_block": 4});
    now["uids"] = json!({"epoch": 1, "block": 2, "rows": [{"uid": 3, "hotkey": "5F"}]});
    let rows = listed(&s, &idx, o.eval.as_ref());
    let mut cohort = value(&cohort_doc(&s, &rows, 1, Some(&sm), &arts, 118, time_of(118)));
    cohort["cohort"]["quantization"] = json!({"rule": "r", "width": "0.01"});
    cohort["cohort"]["admission_closed"] = point.clone();
    for b in ["open", "cutoff", "close"] {
        cohort["cohort"]["bounds"][b] = point.clone();
    }
    cohort["phases"][0]["tempos"] = json!({"elapsed": 1, "counted": 1, "required": 1});
    cohort["quality"]["cutoff"] = json!({"q": null, "rule": null});
    let mut list = value(&cohorts_doc(&s, &idx, &rows, time_of(118)));
    list["records"] = json!([{"first_label": "testnet-rv1-a"}]);
    for r in list["cohorts"].as_array_mut().unwrap() {
        r["quantization"] = json!({"rule": "r", "width": "0.01"});
        r["admission_closed"] = point.clone();
        r["published"] = point.clone();
    }
    for (doc, v) in [(Doc::Now("a"), &now), (Doc::Cohort("a", "c".into()), &cohort), (Doc::Cohorts("a"), &list)] {
        assert_eq!(crate::allowlist::paths(v), allowed_paths(&doc), "{doc:?}");
    }
    assert_eq!(crate::allowlist::paths(&value(&index_doc(&s, 0))), allowed_paths(&Doc::Index));
}

/// The engine logs `transition` when the package commits, inside a tempo;
/// results are published in the next boundary step (`published_epoch`).
/// The stage, its epoch and its time follow the publication boundary and
/// never the commit's minute, in `now`, the cohort list and the cohort
/// document.
#[test]
fn results_published_is_dated_by_its_boundary_not_by_the_package_commit() {
    let c = chain();
    let s = settings(&c);
    let commit_ms = time_of(120) + 433_000;
    let mut log = LOG_SCORING.to_vec();
    log.extend([("packaging", 118), ("transition", 120)]);
    let at = |x: u64, published: Value| {
        let mut e = eval_json(x, "transition", &log, json!({"reward_v1": {"published_epoch": published}}));
        e["cohorts"][1]["log"][6]["at_ms"] = json!(commit_ms);
        observed(e)
    };
    let before = observed(eval_json(120, "packaging", &log[..6], json!({"reward_v1": {"published_epoch": null}})));
    let mut memo = saved_at(119);
    rolled(&before, &mut memo, time_of(119));
    rolled(&before, &mut memo, time_of(120) + 60_000);
    assert_eq!(now_of(&before, &memo).cohort.stage, "packaging");
    // The package has committed mid-tempo: nothing moves until the boundary.
    let committed = at(120, json!(null));
    rolled(&committed, &mut memo, commit_ms + 60_000);
    let d = now_of(&committed, &memo);
    assert_eq!(d.cohort.stage, "packaging");
    assert_eq!(d.cohort.phases, now_of(&before, &memo).cohort.phases);
    let commit_minute = minute(commit_ms);
    assert!(!serde_json::to_string(&d).unwrap().contains(&commit_minute.to_string()));
    // Published at boundary 121, observed in sequence at its first minute.
    let published = at(121, json!(121));
    rolled(&published, &mut memo, time_of(121) + 30_000);
    let d = now_of(&published, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    assert_eq!(d.cohort.stage, "transition");
    let last = d.cohort.phases.last().unwrap();
    assert_eq!((last.epoch, last.at), (Some(121), Some(minute(time_of(121) + 30_000))));
    assert!(!serde_json::to_string(&d).unwrap().contains(&commit_minute.to_string()));
    // Listed after it closed: the publication boundary, undated.
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1), (7, "testnet-r0-c7", None, 1)]);
    let closed = at(124, json!(121));
    let rows = listed(&s, &idx, closed.eval.as_ref());
    assert_eq!(rows[1].row.published, Some(Mark { epoch: Some(121), ..Default::default() }));
    let doc = cohort_doc(&s, &rows, 1, None, &[], 124, time_of(124));
    let text = serde_json::to_string(&doc).unwrap();
    assert!(!text.contains(&commit_minute.to_string()), "{text}");
    // A probe build (its close commits with work) is never in the record.
    let mut e = eval_json(124, "closed", &log, json!({"reward_v1": {"probe_build": true}}));
    e["cohorts"][1]["log"][6]["phase"] = json!("closed");
    let probe = observed(e);
    assert!(!in_record(&probe.eval.as_ref().unwrap().cohorts[1], &s));
}

/// The earliest publication is packaging's earliest boundary: the package
/// can commit in the tempo packaging starts in and publish at the next
/// boundary, so the bound holds against that engine log.
#[test]
fn the_earliest_publication_holds_when_packaging_and_the_package_share_a_tempo() {
    // Boundary 118: scoring is seen (it started in tempo 117).
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({"reward_v1": {"published_epoch": null}})));
    let memo = saved_at(118);
    let d = now_of(&o, &memo);
    let earliest = |d: &NowDoc, stage: &str| {
        d.cohort.plan.as_ref().unwrap().stages.iter().find(|p| p.stage == stage).unwrap().earliest_epoch
    };
    assert_eq!(earliest(&d, "packaging"), Some(119));
    let predicted = earliest(&d, "transition").unwrap();
    // The engine packages and commits the package in tempo 118 and
    // publishes at boundary 119.
    let mut log = LOG_SCORING.to_vec();
    log.extend([("packaging", 118), ("transition", 118)]);
    let o = observed(eval_json(119, "transition", &log, json!({"reward_v1": {"published_epoch": 119}})));
    let d = now_of(&o, &saved_at(119));
    assert_eq!(d.cohort.stage, "transition");
    let actual = d.cohort.phases.last().unwrap().epoch.unwrap();
    assert!(predicted <= actual, "earliest {predicted} after the actual {actual}");
    assert_eq!(predicted, 119);
    // Packaging seen and not yet published: the next boundary at the earliest.
    let o = observed(eval_json(119, "packaging", &log[..6], json!({"reward_v1": {"published_epoch": null}})));
    let d = now_of(&o, &saved_at(119));
    assert_eq!(d.cohort.stage, "packaging");
    assert_eq!(earliest(&d, "transition"), Some(120));
}

/// A token-screen send-back (sealing to building) is not shown: the phases
/// stay monotone in the bar's order.
#[test]
fn a_send_back_never_moves_the_phases_backwards() {
    let log = [
        ("admission", 100),
        ("generating", 116),
        ("sealing", 117),
        ("generating", 117),
        ("sealing", 117),
        ("scoring", 117),
    ];
    let o = observed(eval_json(118, "scoring", &log, json!({})));
    let d = now_of(&o, &saved_at(118));
    let stages: Vec<&str> = d.cohort.phases.iter().map(|p| p.stage.as_str()).collect();
    assert_eq!(stages, ["admission_open", "generating", "sealing", "scoring"]);
    // A send-back in a later tempo: the stage stays at sealing.
    let log = [("admission", 100), ("generating", 116), ("sealing", 117), ("generating", 118)];
    let o = observed(eval_json(119, "generating", &log, json!({})));
    let d = now_of(&o, &saved_at(119));
    let stages: Vec<(&str, Option<u64>)> = d.cohort.phases.iter().map(|p| (p.stage.as_str(), p.epoch)).collect();
    assert_eq!(stages, [("admission_open", Some(100)), ("generating", Some(117)), ("sealing", Some(118))]);
    assert_eq!(d.cohort.progress.work, None);
}

/// The closing step comes from the cohort's own window close (E1), so it
/// stays in `now` once the calendar has moved to the next cohort, and it is
/// in the cohort's document and list row.
#[test]
fn the_closing_step_survives_the_calendar_moving_on() {
    let c = chain();
    let s = settings(&c);
    let mut log = LOG_SCORING.to_vec();
    log.extend([("packaging", 119), ("transition", 120)]);
    let mut e = eval_json(
        121,
        "transition",
        &log,
        json!({"window_closed_epoch": 116, "reward_v1": {"published_epoch": 121}}),
    );
    e["calendar"]["current"] = json!({"cohort": 8, "window_open": false, "window_closing": false});
    let o = observed(e);
    let d = now_of(&o, &saved_at(121));
    let stages: Vec<(&str, Option<u64>)> = d.cohort.phases.iter().map(|p| (p.stage.as_str(), p.epoch)).collect();
    assert_eq!(
        stages,
        [
            ("admission_open", Some(100)),
            ("admission_closing", Some(116)),
            ("generating", Some(117)),
            ("sealing", Some(118)),
            ("scoring", Some(118)),
            ("packaging", Some(120)),
            ("transition", Some(121))
        ]
    );
    // The closed cohort 6 carries its own closing boundary in the list.
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1)]);
    let rows = listed(&s, &idx, o.eval.as_ref());
    assert_eq!(rows[0].row.admission_closed, Some(Mark { epoch: Some(94), ..Default::default() }));
}

/// The summaries and publication records of `entries` in a public bucket.
fn public_with(entries: &[(u64, &str, Option<u8>)]) -> MemStore {
    let public_bucket = MemStore::new();
    for &(n, label, promoted) in entries {
        let sm = summary_for(label, n, 4, promoted);
        public_bucket.insert(&format!("v1/cohorts/{label}/summary.json"), serde_json::to_vec(&sm).unwrap());
        let p = Publication {
            schema: crate::index::PUBLICATION_SCHEMA.into(),
            cohort: n,
            label: label.into(),
            publisher: "test".into(),
            summary: "33".repeat(32),
            artifacts: artifacts_for(&sm),
        };
        public_bucket.insert(&crate::layout::publication(label), serde_json::to_vec(&p).unwrap());
    }
    public_bucket
}

/// A rebuild cut short by the wall budget resumes after the last document
/// it wrote while its inputs are unchanged, and restarts when they change.
#[test]
fn an_interrupted_rebuild_resumes_after_its_last_document() {
    let c = chain();
    let dash = MemStore::new();
    let public_bucket = public_with(&[(5, "testnet-r0-c5", Some(9)), (6, "testnet-r0-c6", None)]);
    let out = DashboardStore::new(&dash);
    let public = PublicStore::new(&public_bucket);
    let idx = index_with(&[(5, "testnet-r0-c5", Some(9), 0), (6, "testnet-r0-c6", None, 1)]);
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let budget = crate::arena::Budget::worker();
    let mut memo = saved_at(118);
    // Each run's wall budget fits one cohort document.
    let run = |memo: &mut Memo, idx: &CohortIndex| {
        let left = std::cell::Cell::new(1u32);
        let within = || {
            let n = left.get();
            left.set(n.saturating_sub(1));
            n > 0
        };
        let inputs = RunInputs {
            settings: settings(&c),
            observed: &o,
            index: Some(idx),
            now_ms: time_of(118),
            budget: &budget,
        };
        block_on(publish(&out, &public, &inputs, memo, saved, &within))
    };
    let cohort_docs = |p: &Pass| p.written.iter().filter(|k| k.contains("/cohorts/testnet")).cloned().collect::<Vec<_>>();
    let p1 = run(&mut memo, &idx);
    assert!(p1.incomplete);
    assert_eq!(cohort_docs(&p1), ["dashboard/v1/m/a/cohorts/testnet-r0-c5.json"]);
    let p2 = run(&mut memo, &idx);
    assert_eq!(cohort_docs(&p2), ["dashboard/v1/m/a/cohorts/testnet-r0-c6.json"], "resumes at c6");
    assert!(!p2.incomplete);
    assert!(memo.cohorts_key.is_some() && memo.pending_key.is_none() && memo.cursor.is_none());
    assert!(cohort_docs(&run(&mut memo, &idx)).is_empty(), "complete: nothing rewritten");
    // Inputs that change during an interrupted rebuild restart it.
    let mut idx2 = idx.clone();
    idx2.cohorts[1].artifacts_pending = 0;
    let p = run(&mut memo, &idx2);
    assert_eq!(cohort_docs(&p), ["dashboard/v1/m/a/cohorts/testnet-r0-c5.json"]);
    let mut idx3 = idx2.clone();
    idx3.cohorts[0].artifacts_pending = 0;
    let p = run(&mut memo, &idx3);
    assert_eq!(cohort_docs(&p), ["dashboard/v1/m/a/cohorts/testnet-r0-c5.json"], "restarted");
}

/// Without the coordinator's status the cohort list and the cohort
/// documents keep their last version: a missing source never rewrites them
/// with fewer facts or drops an ended cohort.
#[test]
fn a_status_outage_rewrites_no_cohort_document() {
    let c = chain();
    let dash = MemStore::new();
    let public_bucket = public_with(&[(6, "testnet-r0-c6", None)]);
    let out = DashboardStore::new(&dash);
    let public = PublicStore::new(&public_bucket);
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1)]);
    let log = [("admission", 100), ("generating", 116), ("abandoned", 117)];
    let live = observed(eval_json(118, "abandoned", &log, json!({})));
    let down = Observed {
        eval: None,
        ..observed(eval_json(118, "abandoned", &log, json!({})))
    };
    let budget = crate::arena::Budget::worker();
    let mut memo = saved_at(118);
    let run = |memo: &mut Memo, o: &Observed| {
        let inputs = RunInputs {
            settings: settings(&c),
            observed: o,
            index: Some(&idx),
            now_ms: time_of(118),
            budget: &budget,
        };
        block_on(publish(&out, &public, &inputs, memo, saved, &|| true))
    };
    let p = run(&mut memo, &live);
    assert!(p.written.contains(&"dashboard/v1/m/a/cohorts/testnet-r0-c7.json".to_owned()), "{:?}", p.written);
    let list = dash.bytes("dashboard/v1/m/a/cohorts/index.json").unwrap();
    let c6 = dash.bytes("dashboard/v1/m/a/cohorts/testnet-r0-c6.json").unwrap();
    let key = memo.cohorts_key.clone();
    let p = run(&mut memo, &down);
    assert_eq!(p.written, ["dashboard/v1/index.json", "dashboard/v1/m/a/now.json"]);
    assert_eq!(dash.bytes("dashboard/v1/m/a/cohorts/index.json").unwrap(), list);
    assert_eq!(dash.bytes("dashboard/v1/m/a/cohorts/testnet-r0-c6.json").unwrap(), c6);
    assert_eq!(memo.cohorts_key, key);
    // The status back: the same inputs, nothing rebuilt.
    let p = run(&mut memo, &live);
    assert_eq!(p.written.len(), 3, "{:?}", p.written);
}

/// The boundary-latched values are saved before a document carries them:
/// when the save fails nothing is written, and a later run in the same
/// tempo publishes one value, which no further run in that tempo changes.
#[test]
fn the_work_percent_is_saved_before_it_is_published() {
    let c = chain();
    let dash = MemStore::new();
    let public_bucket = MemStore::new();
    let out = DashboardStore::new(&dash);
    let public = PublicStore::new(&public_bucket);
    let budget = crate::arena::Budget::worker();
    let at = |done: u64| observed(eval_json(118, "scoring", LOG_SCORING, json!({"progress": {"scoring": {"done": done, "total": 1000}}})));
    let run = |memo: &mut Memo, o: &Observed, now_ms: u64, ok: bool| {
        let inputs = RunInputs {
            settings: settings(&c),
            observed: o,
            index: None,
            now_ms,
            budget: &budget,
        };
        block_on(publish(&out, &public, &inputs, memo, async |_: &Memo| ok, &|| true))
    };
    let pct = || {
        let now: NowDoc = serde_json::from_slice(&dash.bytes("dashboard/v1/m/a/now.json").unwrap()).unwrap();
        now.cohort.progress.work.map(|w| w.pct)
    };
    let stored = saved_at(118);
    // The save fails: nothing written (the state object keeps the old memo).
    let mut m = stored.clone();
    let p = run(&mut m, &at(450), time_of(118) + 60_000, false);
    assert!(p.written.is_empty() && !p.refused.is_empty(), "{p:?}");
    // The run keeps the saved memo, so its final save (which carries the
    // refusal to the alerts) holds no boundary value that was never saved.
    assert_eq!(m, stored);
    // The next run reads the boundary from the same saved memo, saves, and
    // publishes; later runs in the tempo keep that value.
    let mut m = stored.clone();
    run(&mut m, &at(790), time_of(118) + 40 * 60_000, true);
    assert_eq!(pct(), Some(70));
    run(&mut m, &at(950), time_of(118) + 50 * 60_000, true);
    assert_eq!(pct(), Some(70));
    // A lost state mid-tempo publishes no percent until the next boundary.
    let mut lost = Memo::default();
    run(&mut lost, &at(950), time_of(118) + 55 * 60_000, true);
    assert_eq!(pct(), None);
}

// ------------------------------------------------------------ stage times

/// One `boundaries[]` entry of the engine's status for epoch `e`: its block,
/// the time `at` with `basis` (`None` for none), and the credited count the
/// fixture's clock gives it (40 at the opening at 100, one per boundary).
fn bnd(e: u64, at: Option<u64>, basis: Option<&str>) -> Value {
    json!({"epoch": e, "block": block_of(e), "block_basis": "observed", "at_ms": at,
           "at_basis": basis, "active": 40 + e - 100})
}

/// The boundaries the engine reports for cohort 7 up to `x`: observed times
/// at the opening and from 116 on, a derived one between.
fn boundaries_to(x: u64) -> Value {
    let mut v = vec![bnd(100, Some(time_of(100) + 95_000), Some("observed"))];
    for e in 101..=x {
        let basis = if e < 116 { "derived" } else { "observed" };
        v.push(bnd(e, Some(time_of(e) + 20_000), Some(basis)));
    }
    Value::Array(v)
}

/// Every stage is dated by the engine's resolved boundaries, with no memo
/// at all (a publisher that started late): its block, its boundary time to
/// the minute, how that time is known, and its tempos; the current stage
/// counts to the observed boundary; the bounds name the opening, the cutoff
/// and the earliest close.
#[test]
fn stages_are_dated_from_the_engine_boundaries_with_blocks_and_tempos() {
    let mut e = eval_json(118, "scoring", LOG_SCORING, json!({"boundaries": boundaries_to(118), "window_closed_epoch": 116}));
    e["cohorts"][1]["log"].as_array_mut().unwrap().insert(1, json!({"phase": "admission", "at_ms": time_of(116) + 95_000, "epoch": 116}));
    let o = observed(e);
    let memo = Memo::default();
    let d = now_of(&o, &memo);
    let v = checked(&Doc::Now("a"), &d);
    round_trips::<web_model::NowDoc>(&v);
    type Dated<'a> = (&'a str, Option<u64>, Option<u64>, Option<u64>, Option<&'a str>);
    let got: Vec<Dated<'_>> = d
        .cohort
        .phases
        .iter()
        .map(|p| (p.stage.as_str(), p.epoch, p.block, p.at, p.basis.as_deref()))
        .collect();
    let at = |e: u64, plus: u64| Some(minute(time_of(e) + plus));
    assert_eq!(
        got,
        [
            ("admission_open", Some(100), Some(block_of(100)), at(100, 95_000), Some("observed")),
            ("admission_closing", Some(116), Some(block_of(116)), at(116, 20_000), Some("observed")),
            ("generating", Some(117), Some(block_of(117)), at(117, 20_000), Some("observed")),
            ("sealing", Some(118), Some(block_of(118)), at(118, 20_000), Some("observed")),
            ("scoring", Some(118), Some(block_of(118)), at(118, 20_000), Some("observed")),
        ]
    );
    type Counts = Option<(Option<u64>, Option<u64>, Option<u64>)>;
    let t: Vec<Counts> =
        d.cohort.phases.iter().map(|p| p.tempos.as_ref().map(|t| (t.elapsed, t.counted, t.required))).collect();
    assert_eq!(
        t,
        [
            Some((Some(16), Some(16), Some(16))),
            Some((Some(1), Some(1), None)),
            Some((Some(1), Some(1), None)),
            Some((Some(0), Some(0), None)),
            Some((Some(0), Some(0), None)),
        ]
    );
    let b = d.cohort.bounds.as_ref().unwrap();
    assert_eq!(b.open.as_ref().map(|m| (m.epoch, m.block)), Some((Some(100), Some(block_of(100)))));
    assert_eq!(b.cutoff.as_ref().map(|m| m.epoch), Some(Some(116)));
    let close = b.close.as_ref().unwrap();
    assert_eq!((close.epoch, close.block, close.at, close.basis.as_deref()), (d.cohort.expected.close_epoch, d.cohort.expected.close_block, None, Some("earliest")));
    // No work commit's own time (the fixture logs each at the boundary plus
    // 95 s) reaches a work stage.
    for p in &d.cohort.phases[2..] {
        assert_ne!(p.at, at(p.epoch.unwrap() - 1, 95_000), "{}", p.stage);
        assert_ne!(p.at, at(p.epoch.unwrap(), 95_000), "{}", p.stage);
    }
}

/// A boundary the engine has no time for reads as not recorded (no time, no
/// basis), keeping its block; a derived time says so.
#[test]
fn a_boundary_without_a_record_reads_before_tracking_and_derived_times_say_so() {
    let mut bs = boundaries_to(118);
    bs[0] = bnd(100, None, None);
    bs[17]["at_basis"] = json!("derived");
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({"boundaries": bs})));
    let d = now_of(&o, &Memo::default());
    checked(&Doc::Now("a"), &d);
    let first = &d.cohort.phases[0];
    assert_eq!((first.block, first.at, first.basis.as_deref()), (Some(block_of(100)), None, None));
    let generating = d.cohort.phases.iter().find(|p| p.stage == "generating").unwrap();
    assert_eq!((generating.at, generating.basis.as_deref()), (Some(minute(time_of(117) + 20_000)), Some("derived")));
    // From a coordinator without the journal, the publisher's own boundary
    // minute stays the fallback, marked observed, without a block.
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let mut memo = saved_at(118);
    rolled(&o, &mut memo, time_of(118) + 30_000);
    let d = now_of(&o, &memo);
    checked(&Doc::Now("a"), &d);
    let last = d.cohort.phases.last().unwrap();
    assert_eq!((last.block, last.at, last.basis.as_deref()), (None, Some(minute(time_of(118) + 30_000)), Some("observed")));
    assert_eq!(d.cohort.phases[1].at, None, "boundary 117 was never seen by this publisher");
}

/// The results stage needs `max(transition, opened + K - published)`
/// credited tempos: with publication late in the template the K rule
/// governs.
#[test]
fn transition_required_is_effective() {
    let mut log = LOG_SCORING.to_vec();
    log.extend([("packaging", 119), ("transition", 121)]);
    let o = observed(eval_json(
        121,
        "transition",
        &log,
        json!({"opened_active": 50, "published_active": 61, "reward_v1": {"published_epoch": 121}, "boundaries": boundaries_to(124)}),
    ));
    let mut o = o;
    let e = o.eval.as_mut().unwrap();
    *e = parse_eval(&{
        let mut v = eval_json(124, "transition", &log, json!({"opened_active": 50, "published_active": 61,
            "reward_v1": {"published_epoch": 121}, "boundaries": boundaries_to(124)}));
        v["observed"]["active_tempos"] = json!(64);
        v
    });
    let d = now_of(&o, &Memo::default());
    checked(&Doc::Now("a"), &d);
    // Opened at 50 with K = 22 and published at 61: 72 - 61 = 11 > the
    // 2-tempo minimum; 3 credited since publication.
    assert_eq!(d.cohort.tempos, Some(Tempos { credited: Some(3), required: Some(11) }));
    let last = d.cohort.phases.last().unwrap();
    assert_eq!(last.tempos, Some(PhaseTempos { elapsed: Some(3), counted: Some(3), required: Some(11) }));
}

/// A closed cohort's document carries its bounds and its tempo length; the
/// list's marks carry blocks and bases but no bounds; the close is an
/// instant.
#[test]
fn cohort_documents_carry_bounds_and_their_tempo_length() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1)]);
    let mut e = eval_json(118, "scoring", LOG_SCORING, json!({}));
    let c6: Vec<Value> = [78, 94, 95, 96, 98, 99, 100].iter().map(|&x| bnd(x, Some(time_of(x) + 20_000), Some("observed"))).collect();
    e["cohorts"][0]["boundaries"] = Value::Array(c6);
    let o = observed(e);
    let rows = listed(&s, &idx, o.eval.as_ref());
    let d = cohort_doc(&s, &rows, 0, None, &[], 118, time_of(118));
    let v = checked(&Doc::Cohort("a", "testnet-r0-c6".into()), &d);
    round_trips::<web_model::CohortDoc>(&v);
    assert_eq!(d.cohort.tempo_blocks, Some(TEMPO));
    let b = d.cohort.bounds.as_ref().unwrap();
    let close = b.close.as_ref().unwrap();
    assert_eq!((close.epoch, close.block, close.basis.as_deref()), (Some(100), Some(block_of(100)), Some("observed")));
    assert_eq!(b.cutoff.as_ref().map(|m| m.epoch), Some(Some(94)));
    assert_eq!(d.phases.last().map(|p| (p.stage.as_str(), p.tempos.is_none())), Some(("closed", true)));
    assert!(d.phases.iter().all(|p| p.at.is_some() && p.block.is_some()), "{:?}", d.phases);
    let list = cohorts_doc(&s, &idx, &rows, time_of(118));
    let v = checked(&Doc::Cohorts("a"), &list);
    round_trips::<web_model::CohortsDoc>(&v);
    let r = &list.cohorts[0];
    assert_eq!((r.bounds.clone(), r.tempo_blocks), (None, Some(TEMPO)));
    assert_eq!(r.anchor, Some(Anchor { epoch: Some(78), block: Some(block_of(78)) }), "the clock projected to the opening");
    assert_eq!(r.opened.as_ref().map(|m| (m.block, m.basis.as_deref())), Some((Some(block_of(78)), Some("observed"))));
    // A block off the clock's arithmetic (a tempo change, or a block inside
    // a tempo) drops on its own; the others keep theirs.
    let mut e = eval_json(118, "scoring", LOG_SCORING, json!({}));
    let mut c6: Vec<Value> = [78, 94, 95, 96, 98, 99, 100].iter().map(|&x| bnd(x, Some(time_of(x) + 20_000), Some("observed"))).collect();
    c6[0]["block"] = json!(block_of(78) + 16 * 2);
    e["cohorts"][0]["boundaries"] = Value::Array(c6);
    let o = observed(e);
    let rows = listed(&s, &idx, o.eval.as_ref());
    let d = cohort_doc(&s, &rows, 0, None, &[], 118, time_of(118));
    checked(&Doc::Cohort("a", "testnet-r0-c6".into()), &d);
    assert_eq!(d.cohort.tempo_blocks, Some(TEMPO));
    assert_eq!(d.phases[0].block, None);
    assert_eq!(d.phases[1].block, Some(block_of(94)));
    // Without the clock's boundary there is no anchor and no block.
    let mut e = eval_json(118, "scoring", LOG_SCORING, json!({}));
    e["cohorts"][0]["boundaries"] = Value::Array(
        [78, 94, 95, 96, 98, 99, 100].iter().map(|&x| bnd(x, Some(time_of(x) + 20_000), Some("observed"))).collect(),
    );
    let mut o = observed(e);
    o.eval.as_mut().unwrap().boundary_block = None;
    let rows = listed(&s, &idx, o.eval.as_ref());
    let d = cohort_doc(&s, &rows, 0, None, &[], 118, time_of(118));
    checked(&Doc::Cohort("a", "testnet-r0-c6".into()), &d);
    assert_eq!((d.cohort.tempo_blocks, d.cohort.anchor.clone()), (None, None));
    assert!(d.phases.iter().all(|p| p.block.is_none() && p.at.is_some()));
}

/// Values a correct build never writes are dropped by the builder's checks
/// and refused by [`check`]: a block inside a tempo, a time off the minute or
/// after the document, a basis outside the contract, wrong tempos, a time
/// field outside the contract, and times that go backwards.
#[test]
fn stage_times_outside_the_contract_are_refused() {
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({"boundaries": boundaries_to(118)})));
    let good = checked(&Doc::Now("a"), &now_of(&o, &Memo::default()));
    let key = Doc::Now("a").key();
    let refused = |mutate: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        mutate(&mut v);
        check(&key, &serde_json::to_vec(&v).unwrap()).is_err()
    };
    let ph = |v: &mut Value, i: usize, k: &str, x: Value| v["cohort"]["phases"][i][k] = x;
    assert!(refused(&|v| ph(v, 2, "block", json!(block_of(117) + 5))), "a block inside a tempo");
    assert!(refused(&|v| ph(v, 2, "at", json!(minute(time_of(117)) + 7))), "off the minute");
    assert!(refused(&|v| ph(v, 2, "at", json!(minute(time_of(140))))), "after the document");
    assert!(refused(&|v| ph(v, 2, "basis", json!("guessed"))));
    assert!(refused(&|v| ph(v, 2, "basis", json!("earliest"))));
    assert!(refused(&|v| ph(v, 2, "at", json!(minute(time_of(99))))), "backwards");
    assert!(refused(&|v| ph(v, 2, "started_ms", json!(1))), "a time field outside the contract");
    assert!(refused(&|v| v["cohort"]["phases"][2]["tempos"]["elapsed"] = json!(3)));
    assert!(refused(&|v| v["cohort"]["phases"][2]["tempos"]["counted"] = json!(4)));
    assert!(refused(&|v| v["cohort"]["phases"][2]["tempos"]["required"] = json!(4)));
    assert!(refused(&|v| v["cohort"]["phases"][2]["tempos"]["at_ms"] = json!(4)));
    assert!(refused(&|v| v["cohort"]["bounds"]["close"]["basis"] = json!("observed")));
    assert!(refused(&|v| v["cohort"]["bounds"]["close"]["epoch"] = json!(200)));
    assert!(refused(&|v| v["cohort"]["bounds"]["open"]["epoch"] = json!(101)));
    assert!(refused(&|v| v["cohort"]["bounds"]["open"]["observed_ms"] = json!(1)));
    assert!(!refused(&|_| {}));
    // The builder's own checks drop rather than refuse. In `now.json` a block
    // off the clock's boundary arithmetic is dropped on its own ...
    let mut d = now_of(&o, &Memo::default());
    assert_eq!(d.cohort.phases[1].stage, "generating");
    d.cohort.phases[1].block = Some(block_of(117) + 5);
    clean_now(&mut d);
    let blocks: Vec<bool> = d.cohort.phases.iter().map(|p| p.block.is_some()).collect();
    assert_eq!(blocks, [true, false, true, true]);
    assert!(d.cohort.phases.iter().all(|p| p.at.is_some()), "times stay");
    // ... and in a cohort document a block off the row's anchor drops on its
    // own.
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", None, 1)]);
    let mut e = eval_json(118, "scoring", LOG_SCORING, json!({}));
    let c6: Vec<Value> = [78, 94, 95, 96, 98, 99, 100].iter().map(|&x| bnd(x, Some(time_of(x) + 20_000), Some("observed"))).collect();
    e["cohorts"][0]["boundaries"] = Value::Array(c6);
    let o = observed(e);
    let rows = listed(&s, &idx, o.eval.as_ref());
    let good = cohort_doc(&s, &rows, 0, None, &[], 118, time_of(118));
    let mut d = good.clone();
    d.phases[2].block = Some(block_of(95) + 5);
    clean_cohort(&mut d);
    assert_eq!(d.phases[2].block, None);
    assert!(d.phases.iter().all(|p| p.at.is_some()));
    assert!(d.cohort.opened.as_ref().unwrap().block.is_some());
    // Leak review 1: every block of a cohort document shifted together (or
    // with no anchor, or a phase moved inside a tempo) is refused.
    let key = Doc::Cohort("a", "testnet-r0-c6".into()).key();
    let doc_refused = |mutate: &dyn Fn(&mut Value)| {
        let mut v = value(&good);
        mutate(&mut v);
        check(&key, &serde_json::to_vec(&v).unwrap()).is_err()
    };
    assert!(!doc_refused(&|_| {}));
    let shift = |v: &mut Value| {
        fn walk(v: &mut Value) {
            match v {
                Value::Object(m) => {
                    for (k, x) in m.iter_mut() {
                        if k == "block" && x.is_u64() {
                            *x = json!(x.as_u64().unwrap() + 5);
                        } else if k != "anchor" {
                            walk(x);
                        }
                    }
                }
                Value::Array(a) => a.iter_mut().for_each(walk),
                _ => {}
            }
        }
        walk(v);
    };
    assert!(doc_refused(&shift), "all blocks shifted against the anchor");
    assert!(doc_refused(&|v| v["cohort"]["anchor"] = Value::Null), "blocks without an anchor");
    assert!(doc_refused(&|v| v["cohort"]["tempo_blocks"] = json!(122)), "a self-declared tempo");
    // Correctness review 5: a row mark that disagrees with the phase at its
    // epoch is refused by the publisher, as the website would drop it.
    assert!(doc_refused(&|v| v["cohort"]["opened"]["at"] = json!(minute(time_of(78)) - 60)));
    // Leak review 2: a stage after the cohort's close.
    assert!(doc_refused(&|v| {
        let extra = json!({"stage": "scoring", "epoch": 100, "block": null, "at": null, "basis": null, "tempos": null});
        v["phases"].as_array_mut().unwrap().push(extra);
    }));
}

/// Leak review 2: `now.json` never dates a stage or a recorded bound after
/// the clock's boundary.
#[test]
fn now_refuses_a_stage_after_the_clocks_boundary() {
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({"boundaries": boundaries_to(118)})));
    let good = checked(&Doc::Now("a"), &now_of(&o, &Memo::default()));
    let key = Doc::Now("a").key();
    let refused = |mutate: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        mutate(&mut v);
        check(&key, &serde_json::to_vec(&v).unwrap()).is_err()
    };
    let x = good["clock"]["boundary_epoch"].as_u64().unwrap();
    let n = good["cohort"]["phases"].as_array().unwrap().len();
    assert!(refused(&|v| {
        v["cohort"]["phases"][n - 1]["epoch"] = json!(x + 1);
        v["cohort"]["phases"][n - 1]["block"] = json!(block_of(x + 1));
        v["cohort"]["phases"][n - 1]["tempos"] = Value::Null;
    }));
    assert!(refused(&|v| {
        v["cohort"]["bounds"]["close"]["basis"] = json!("observed");
        v["cohort"]["bounds"]["close"]["at"] = json!(minute(time_of(x)));
    }));
    // The earliest close is a future estimate by design.
    assert!(good["cohort"]["bounds"]["close"]["epoch"].as_u64().unwrap() > x);
}

/// Correctness review 3: a boundary the engine could not date (records on
/// both sides, none supporting a time) is `unresolved`, not before tracking.
#[test]
fn an_underivable_boundary_is_unresolved() {
    let mut b = boundaries_to(118);
    let arr = b.as_array_mut().unwrap();
    let k = arr.iter().position(|x| x["epoch"] == 117).unwrap();
    arr[k] = json!({"epoch": 117, "block": null, "block_basis": null, "at_ms": null, "at_basis": null,
                    "active": null, "at_reason": "not_derivable"});
    let k0 = arr.iter().position(|x| x["epoch"] == 100).unwrap();
    arr[k0] = json!({"epoch": 100, "block": null, "block_basis": null, "at_ms": null, "at_basis": null,
                     "active": null, "at_reason": "before_tracking"});
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({"boundaries": b})));
    let d = now_of(&o, &Memo::default());
    checked(&Doc::Now("a"), &d);
    let at = |st: &str| d.cohort.phases.iter().find(|p| p.stage == st).map(|p| (p.at, p.basis.clone())).unwrap();
    assert_eq!(at("generating"), (None, Some("unresolved".into())), "seen at 117");
    assert_eq!(at("admission_open"), (None, None), "before tracking: no basis");
}

/// Correctness review 4: an admission that ends without reaching its
/// minimum (abandoned) counts its credited tempos by the records, never the
/// minimum.
#[test]
fn an_abandoned_admission_counts_by_the_records() {
    let log = [("admission", 100), ("abandoned", 117)];
    let mut b = boundaries_to(118);
    for x in b.as_array_mut().unwrap() {
        // Only every other boundary credited after 104.
        let e = x["epoch"].as_u64().unwrap();
        x["active"] = json!(40 + (e - 100).min(4) + (e.saturating_sub(104)) / 2);
    }
    let o = observed(eval_json(118, "abandoned", &log, json!({"boundaries": b})));
    let c = o.eval.as_ref().unwrap().cohorts.iter().find(|c| c.cohort == 7).unwrap();
    let mut phases = visible_phases(c, 118, None, &BTreeMap::new());
    assert_eq!(phases.iter().map(|p| p.stage.as_str()).collect::<Vec<_>>(), ["admission_open", "ended"]);
    with_tempos(c, &mut phases, None);
    let t = phases[0].tempos.clone().unwrap();
    // The end commits inside epoch 117 and is seen at 118: 18 epochs, of
    // which 4 + 14 / 2 = 11 credited.
    assert_eq!(t.elapsed, Some(18));
    assert_eq!(t.counted, Some(11), "credited by the records, not the 16-tempo minimum");
}

/// The website's fixtures satisfy the same stage-time checks the publisher
/// runs before writing.
#[test]
fn the_websites_fixtures_pass_the_stage_time_checks() {
    fn ok<T: Clone + PartialEq + std::fmt::Debug + for<'d> Deserialize<'d>>(name: &str, text: &str, clean: fn(&mut T)) {
        let d: T = serde_json::from_str(text).unwrap();
        let mut c = d.clone();
        clean(&mut c);
        assert_eq!(c, d, "{name}");
    }
    macro_rules! fixtures {
        ($t:ty, $f:expr, $($name:literal),+) => {
            $(ok::<$t>($name, include_str!(concat!("../tests/fixtures/dashboard-web/", $name, ".json")), $f);)+
        };
    }
    fixtures!(NowDoc, clean_now, "now-admission", "now-closing", "now-building", "now-scoring", "now-transition",
        "now-queued", "now-idle", "now-paused", "now-hold", "now-epoch-only");
    fixtures!(CohortsDoc, clean_cohorts, "cohorts-scoring");
    fixtures!(CohortDoc, clean_cohort, "cohort-scoring-testnet-c117", "cohort-scoring-testnet-c112",
        "cohort-transition-testnet-c118", "cohort-no-scores-testnet-c117");
}

/// A score is published only under a quantization block and only on its
/// grid: one without the block, with an unknown rule, with more decimals
/// than the width or off its multiples is refused; a band's upper edge may
/// be the scale's 1.
#[test]
fn scores_are_refused_off_the_grid_or_without_a_quantization_block() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(6, "testnet-r0-c6", Some(1), 1)]);
    let rows = listed(&s, &idx, None);
    let sm = summary_for("testnet-r0-c6", 6, 4, Some(1));
    let arts = artifacts_for(&sm);
    let d = cohort_doc(&s, &rows, 0, Some(&sm), &arts, 0, time_of(118));
    let key = Doc::Cohort("a", "testnet-r0-c6".into()).key();
    let good = value(&d);
    check(&key, &serde_json::to_vec(&good).unwrap()).unwrap();
    let refused = |edit: &dyn Fn(&mut Value)| {
        let mut v = good.clone();
        edit(&mut v);
        check(&key, &serde_json::to_vec(&v).unwrap()).unwrap_err()
    };
    assert!(refused(&|v| v["cohort"]["quantization"] = Value::Null).contains("without a quantization block"));
    assert!(refused(&|v| v["cohort"]["quantization"]["rule"] = json!("quant-v0")).contains("quantization rule"));
    assert!(refused(&|v| v["cohort"]["quantization"]["width"] = json!("0.021")).contains("quantization width"));
    assert!(refused(&|v| v["entrants"][0]["score"] = json!("0.901")).contains("off the grid"));
    assert!(refused(&|v| v["entrants"][0]["score"] = json!("0.91")).contains("off the grid"));
    assert!(refused(&|v| v["reference"]["score"] = json!("0.8400")).contains("off the grid"));
    assert!(refused(&|v| v["quality"]["cutoff"]["q"] = json!("1.01")).contains("off the grid"));
    let mut top = good.clone();
    top["quality"]["bands"][1]["upper"] = json!("1.00");
    check(&key, &serde_json::to_vec(&top).unwrap()).unwrap();
}

/// Review 2026-11-05 (correctness 4): a cluster is placed by its head, so a
/// member with a lower `q` (here in a lower grid band, where another
/// cluster's head sits at the same `q`) keeps its cluster's band label while
/// its dot falls below that band's lower edge. The edges stay the heads'
/// grid edges; only the heads are guaranteed inside them.
#[test]
fn a_cluster_member_in_a_lower_grid_band_keeps_its_cluster_band() {
    let quality = summary::Quality {
        cutoff: Some(summary::Cutoff { q: "0.60".into(), mode: "margin".into() }),
        bands: vec![
            summary::Band { band: 1, lower: "0.66".into(), upper: "0.69".into() },
            summary::Band { band: 2, lower: "0.63".into(), upper: "0.66".into() },
        ],
    };
    let edges = band_edges(&quality);
    let st = |t: &str| Some(t.to_owned());
    assert_eq!(
        edges.iter().map(|b| (b.band, b.lower.clone(), b.upper.clone())).collect::<Vec<_>>(),
        [(1, st("0.66"), None), (2, st("0.60"), st("0.66"))]
    );
    let w = 30_000_000_000_000_000;
    let at = |t: &str| slop_ninja_protocol::quant::parse_grid(t, w).unwrap();
    // Head of band 1 at 0.66, its member at 0.63, band 2's head at 0.63.
    let (head, member, other_head) = (at("0.66"), at("0.63"), at("0.63"));
    let lower = |b: usize| at(edges[b].lower.as_deref().unwrap());
    assert!(head >= lower(0), "the head is inside its band");
    assert!(other_head >= lower(1) && other_head < at("0.66"), "so is band 2's head");
    assert!(member < lower(0), "the member's dot is below its cluster's band edge, as documented");
}

/// Review 2026-11-05 (ops 4): a cohort listed before quantization keeps its
/// `cohort_summary/v2`. Its document still lists the entrants and occupied
/// bands, with every score, edge, cutoff and `quantization` null.
#[test]
fn a_cohort_listed_before_quantization_keeps_entrants_and_bands_without_scores() {
    let c = chain();
    let s = settings(&c);
    let idx = index_with(&[(5, "testnet-r0-c5", None, 1), (6, "testnet-r0-c6", Some(1), 1)]);
    let o = observed(eval_json(118, "scoring", LOG_SCORING, json!({})));
    let rows = listed(&s, &idx, o.eval.as_ref());
    let mut v = serde_json::to_value(summary_for("testnet-r0-c6", 6, 4, Some(1))).unwrap();
    v["schema"] = summary::SUMMARY_SCHEMA_V2.into();
    let o = v.as_object_mut().unwrap();
    o.remove("quantization");
    o.remove("quality");
    for r in ["current", "predecessor"] {
        if let Some(m) = v["references"][r].as_object_mut() {
            m.remove("q");
        }
    }
    for e in v["entrants"].as_array_mut().unwrap() {
        let e = e.as_object_mut().unwrap();
        e.remove("q");
        let copy = e["copy_gate"].as_str().unwrap_or("pass").to_owned();
        e.insert("copy_gate".into(), copy.into());
    }
    let bytes = serde_json::to_vec(&v).unwrap();
    crate::allowlist::check_object("v1/cohorts/testnet-r0-c6/summary.json", &bytes).unwrap();
    let sm = summary::read(&bytes).expect("v2 reads");
    let arts = artifacts_for(&sm);
    let d = cohort_doc(&s, &rows, 1, Some(&sm), &arts, 118, time_of(118));
    checked(&Doc::Cohort("a", "testnet-r0-c6".into()), &d);
    assert!(d.cohort.leaderboard);
    assert_eq!(d.cohort.quantization, None);
    assert_eq!(d.cohort.best_score, None);
    let en = d.entrants.as_ref().unwrap();
    assert_eq!(en.len(), 4);
    assert!(en.iter().all(|e| e.score.is_none()));
    assert!(en.iter().any(|e| e.band.is_some()));
    let q = d.quality.as_ref().unwrap();
    assert_eq!(q.cutoff, None);
    let bands = q.bands.as_ref().unwrap();
    assert!(!bands.is_empty());
    assert!(bands.iter().all(|b| b.lower.is_none() && b.upper.is_none()));
    assert_eq!(d.reference.as_ref().unwrap().score, None);
}

#[path = "dashboard_roster_tests.rs"]
mod roster_tests;

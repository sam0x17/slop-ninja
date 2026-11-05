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
    B0 + epoch * (TEMPO + 1)
}

fn time_of(epoch: u64) -> u64 {
    T0_MS + epoch * (TEMPO + 1) * 12_000
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

fn now_of(o: &Observed, memo: &Memo) -> NowDoc {
    let c = chain();
    now_doc(&settings(&c), o, None, None, memo, time_of(110))
}

fn value(v: &impl Serialize) -> Value {
    serde_json::to_value(v).unwrap()
}

/// The document round-trips through the website's types unchanged: no field
/// the website would drop, none it lacks.
fn round_trips<T: Serialize + for<'d> Deserialize<'d>>(v: &Value) {
    let parsed: T = serde_json::from_value(v.clone()).expect("the website's types parse it");
    assert_eq!(&value(&parsed), v, "round trip through the website's model");
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
        });
    }
    idx
}

fn summary_for(label: &str, cohort: u64, n: usize, promoted: Option<u8>) -> CohortSummary {
    let c = chain();
    let model = |b: u8| ModelRef {
        content: format!("{b:02x}").repeat(32),
        artifact: format!("{b:02x}").repeat(32),
    };
    CohortSummary {
        schema: summary::SUMMARY_SCHEMA.into(),
        network: "testnet".into(),
        chain: slop_ninja_admission_wire::json::Chain::from_binding(&c),
        cohort,
        label: label.into(),
        opened_epoch: 78,
        closed_epoch: 100,
        references: References {
            index: 1,
            current: model(0xaa),
            predecessor: Some(model(0xbb)),
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
                    band: match i % 4 {
                        0 => Some(1),
                        1 => Some(2),
                        2 => None,
                        _ => Some(1),
                    },
                    gate: if i % 4 == 2 { "not_executed" } else { "pass" }.into(),
                    copy_gate: "pass".into(),
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
            path: (i == 0).then(|| format!("v1/artifacts/{}.bin", format!("{:02x}", i % 256).repeat(32))),
            reason: Some("PRIVATE-DOCUMENT-TEXT".into()),
            hotkey_hex: "ab".repeat(32),
            registration_block: 7,
            commitment: "dd".repeat(32),
            nonce: "ee".repeat(16),
        })
        .collect()
}

#[test]
fn a_closed_cohort_document_carries_bands_and_gates_but_no_score_or_rank() {
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
    for e in en {
        assert_eq!(e.eligible, e.band.is_some());
        assert_eq!(e.score, None);
        assert_eq!(e.row_share_pct, None);
    }
    assert!(!en[2].gates_passed && en[0].gates_passed);
    assert!(en[0].promoted && !en[1].promoted);
    assert_eq!(en[0].artifact.as_ref().unwrap().state, "published");
    assert!(en[0].artifact.as_ref().unwrap().download.as_deref().unwrap().starts_with("v1/artifacts/"));
    assert_eq!(en[1].artifact.as_ref().unwrap().download, None);
    let q = d.quality.as_ref().unwrap();
    assert_eq!(q.cutoff, None);
    assert_eq!(q.bands.as_ref().unwrap().iter().map(|b| (b.band, b.lower.clone(), b.upper.clone())).collect::<Vec<_>>(), [(1, None, None), (2, None, None)]);
    assert_eq!(d.reference.as_ref().map(|r| (r.index, r.label.as_str(), r.score.clone())), Some((1, "R1", None)));
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
    assert_eq!((b.latest_score, b.quantization, b.licence), (None, None, None));
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
    padded.resize(contract::bounds::SMALL + 1, b' ');
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
    let pass = run(&mut memo, &o, &idx2, true);
    assert_eq!(pass.written.len(), 5, "{:?}", pass.written);
    // Out of wall time: the rebuild stops and resumes next run.
    idx2.cohorts[1].artifacts_pending = 0;
    let pass = run(&mut memo, &o, &idx2, false);
    assert!(pass.incomplete && pass.written.len() == 3);
    let pass = run(&mut memo, &o, &idx2, true);
    assert_eq!(pass.written.len(), 5);
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
    let rows = listed(&s, &idx, o.eval.as_ref());
    let mut cohort = value(&cohort_doc(&s, &rows, 1, Some(&sm), &arts, 118, time_of(118)));
    cohort["cohort"]["quantization"] = json!({"rule": "r", "width": "0.01"});
    cohort["cohort"]["admission_closed"] = json!({"epoch": 1, "at": 60});
    cohort["quality"]["cutoff"] = json!({"q": null, "rule": null});
    let mut list = value(&cohorts_doc(&s, &idx, &rows, time_of(118)));
    list["records"] = json!([{"first_label": "testnet-rv1-a"}]);
    for r in list["cohorts"].as_array_mut().unwrap() {
        r["quantization"] = json!({"rule": "r", "width": "0.01"});
        r["admission_closed"] = json!({"epoch": 1, "at": 60});
        r["published"] = json!({"epoch": 1, "at": 60});
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
    assert_eq!(rows[1].row.published, Some(Mark { epoch: Some(121), at: None }));
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
    assert_eq!(rows[0].row.admission_closed, Some(Mark { epoch: Some(94), at: None }));
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

//! The registry and roster documents (bounds rule v3): built only from the
//! coordinator's registry read, the eval Worker's per-hotkey admission rows,
//! the published artifacts record and the public summary; whole or nothing;
//! no score, rank or result before close; the running roster only at the
//! configured level.

use super::*;
use crate::artifacts::{StartEntry, StartRecord};
use crate::status::RegistryView;

fn hx(n: u8) -> String {
    hex::encode([n; 32])
}

fn ss(n: u8) -> String {
    crate::ss58::encode(&AccountId([n; 32]))
}

fn registry_json(rows: Value) -> Value {
    json!({"rule": crate::status::REGISTRY_RULE, "epoch": 118, "block": block_of(118) + 40, "rows": rows})
}

fn coordinator_with(registry: Option<Value>) -> crate::status::WorkerView {
    let mut w = worker_status("primary", false);
    if let Some(r) = registry {
        w["chain_journal"]["registry"] = r;
    }
    parse_worker(&w)
}

#[test]
fn the_registry_is_read_whole_or_not_at_all() {
    let good = registry_json(json!([
        {"uid": 0, "hotkey": hx(1), "registration_block": 10},
        {"uid": 4, "hotkey": hx(2), "registration_block": block_of(118)}
    ]));
    let w = coordinator_with(Some(good.clone()));
    let view = w.registry.clone().flatten().expect("a whole read");
    assert_eq!(view.rows, vec![(0, [1; 32], 10), (4, [2; 32], block_of(118))]);
    // The rest of the status is unchanged by the new field.
    assert_eq!(w.mode.as_deref(), Some("primary"));
    assert_eq!(w.signing.as_ref().map(|s| s.ops.len()), Some(1));
    // A coordinator built before the field: nothing reported.
    assert_eq!(coordinator_with(None).registry, None);
    let bad = |f: &dyn Fn(&mut Value)| {
        let mut r = good.clone();
        f(&mut r);
        let w = coordinator_with(Some(r));
        assert_eq!(w.mode.as_deref(), Some("primary"), "the status still parses");
        w.registry
    };
    assert_eq!(bad(&|r| *r = Value::Null), Some(None), "null before any read");
    assert_eq!(bad(&|r| r["rows"] = Value::Null), Some(None), "refused over the bound");
    assert_eq!(bad(&|r| r["rule"] = json!("slop_ninja.coordinator.registry/v0")), Some(None));
    assert_eq!(bad(&|r| r["rows"][1]["uid"] = json!(0)), Some(None), "a repeated UID");
    assert_eq!(bad(&|r| r["rows"][1]["hotkey"] = json!(hx(1))), Some(None), "a repeated hotkey");
    assert_eq!(bad(&|r| r["rows"][1]["hotkey"] = json!(hx(0xab).to_uppercase())), Some(None), "not lowercase");
    assert_eq!(bad(&|r| r["rows"][1]["registration_block"] = json!(block_of(119))), Some(None), "after the block");
    assert_eq!(bad(&|r| r["rows"][1]["uid"] = json!(70_000)), Some(None), "outside u16");
    assert_eq!(bad(&|r| r["rows"] = json!("all of them")), Some(None), "malformed, not fatal");
    assert_eq!(bad(&|r| r["rows"][0] = json!({"uid": 0})), Some(None), "a row missing a field");
    let over: Vec<Value> = (0..=contract::registry::ROWS as u16)
        .map(|u| json!({"uid": u, "hotkey": hex::encode([&[(u >> 8) as u8, u as u8][..], &[1; 30]].concat()), "registration_block": 1}))
        .collect();
    assert_eq!(bad(&|r| r["rows"] = json!(over)), Some(None), "more rows than the bound");
}

#[test]
fn the_registry_document_carries_both_spellings_and_fits_its_bound() {
    let c = chain();
    let s = settings(&c);
    let view = RegistryView {
        epoch: 118,
        block: block_of(118) + 40,
        rows: vec![(0, [1; 32], 10), (4, [2; 32], 11)],
    };
    let d = registry_doc(&s, Some(&view), time_of(118));
    let v = checked(&Doc::Registry, &d);
    assert_eq!(
        v["rows"],
        json!([{"uid": 0, "hotkey": ss(1), "hotkey_hex": hx(1), "registration_block": 10},
               {"uid": 4, "hotkey": ss(2), "hotkey_hex": hx(2), "registration_block": 11}])
    );
    assert_eq!(crate::allowlist::paths(&v), allowed_paths(&Doc::Registry), "the allow-list is exactly the document");
    // No whole read: every fact null, still a valid document.
    let none = checked(&Doc::Registry, &registry_doc(&s, None, time_of(118)));
    assert_eq!((none["epoch"].clone(), none["rows"].clone()), (Value::Null, Value::Null));
    // Refusals: spellings that disagree, order, a boundary without rows.
    let refused = |f: &dyn Fn(&mut Value)| {
        let mut x = v.clone();
        f(&mut x);
        check(&Doc::Registry.key(), &serde_json::to_vec(&x).unwrap()).is_err()
    };
    assert!(refused(&|x| x["rows"][1]["hotkey"] = json!(ss(3))));
    assert!(refused(&|x| x["rows"][1]["uid"] = json!(0)));
    assert!(refused(&|x| x["rows"] = Value::Null));
    assert!(refused(&|x| x["rows"][0]["registration_block"] = json!(block_of(119))));
    assert!(refused(&|x| x["rows"][0]["score"] = json!("0.9")));
    assert!(refused(&|x| x["rule"] = json!("x")));
    // A full read of the longest rows fits the bound.
    let full = RegistryView {
        epoch: u64::MAX,
        block: u64::MAX,
        rows: (0..contract::registry::ROWS as u16)
            .map(|u| {
                let uid = u16::MAX - (contract::registry::ROWS as u16 - 1) + u;
                let mut k = [0xff; 32];
                k[..2].copy_from_slice(&u.to_be_bytes());
                (uid, k, u64::MAX)
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&registry_doc(&s, Some(&full), time_of(118))).unwrap();
    let rows = serde_json::to_vec(&registry_doc(&s, Some(&full), time_of(118)).rows).unwrap();
    assert!(rows.len() <= contract::registry::ROWS * contract::registry::ROW_BYTES, "{} bytes", rows.len());
    assert!(bytes.len() <= contract::bounds::REGISTRY);
    check(&Doc::Registry.key(), &bytes).unwrap();
}

/// A summary with one entrant of each gate outcome and a cluster.
fn mixed_summary() -> CohortSummary {
    let mut sm = summary_for("testnet-r0-c6", 6, 4, Some(1));
    // Roster order is not UID order.
    for (e, uid) in sm.entrants.iter_mut().zip([9u16, 2, 5, 7]) {
        e.uid = uid;
    }
    sm.entrants[1].gate = "fail".into();
    sm.entrants[1].copy_gate = Some("fail".into());
    sm.entrants[3].gate = "withheld".into();
    sm.entrants[3].copy_gate = None;
    sm.entrants[3].q = None;
    sm.entrants[3].band = None;
    sm.entrants[0].cluster = Some(summary::ClusterRef {
        uid: 9,
        hotkey: sm.entrants[0].hotkey.clone(),
    });
    sm
}

#[test]
fn a_closed_roster_carries_the_summarys_bits_in_uid_order_and_no_score() {
    let c = chain();
    let s = settings(&c);
    let sm = mixed_summary();
    let arts = artifacts_for(&sm);
    let d = closed_roster(&s, &sm, &arts, time_of(118));
    let v = checked(&Doc::Roster("a", "testnet-r0-c6".into()), &d);
    let uids: Vec<Option<u16>> = d.rows.iter().map(|r| r.uid).collect();
    assert_eq!(uids, [Some(2), Some(5), Some(7), Some(9)], "UID order, never roster or score order");
    let by_uid = |u: u16| d.rows.iter().find(|r| r.uid == Some(u)).unwrap();
    assert_eq!(by_uid(9).roster_position, Some(1));
    assert_eq!(by_uid(9).status, "scored");
    assert_eq!(by_uid(9).artifact_state.as_deref(), Some("published"));
    assert_eq!(by_uid(9).result.as_ref().unwrap().cluster.as_ref().map(|c| c.uid), Some(9));
    assert_eq!(by_uid(2).status, "scored");
    assert_eq!(by_uid(2).result.as_ref().unwrap().copy_gate.as_deref(), Some("fail"));
    assert_eq!(by_uid(5).status, "not_executed", "withheld and not executed stay apart");
    assert_eq!(by_uid(7).status, "withheld");
    assert_eq!(by_uid(7).result.as_ref().unwrap().copy_gate, None);
    for r in &d.rows {
        let hex_of = crate::ss58::decode(&r.hotkey).map(|a| hex::encode(a.0));
        assert_eq!(r.hotkey_hex, hex_of, "both spellings of one key");
    }
    let refs = d.references.as_ref().unwrap();
    assert_eq!((refs.index, refs.current.content.as_str()), (1, "aa".repeat(32).as_str()));
    assert_eq!(d.source, "summary");
    assert_eq!(d.level, "closed");
    // No score anywhere: no q, cutoff, band edge or score key.
    let text = serde_json::to_string(&v).unwrap();
    for word in ["\"q\"", "\"score\"", "score_", "_score", "cutoff", "lower", "upper", "row_share", "eligible", "0.86", "0.90", "0.84"] {
        assert!(!text.contains(word), "{word} in {text}");
    }
    assert_eq!(crate::allowlist::paths(&v).iter().filter(|p| p.contains("score") || p.ends_with(".q")).count(), 0);
    // The document names only its own cohort.
    let wrong = check(&Doc::Roster("a", "testnet-r0-c5".into()).key(), &serde_json::to_vec(&d).unwrap());
    assert!(wrong.is_err());
    // A full roster of the longest rows fits the bound.
    let big = summary_for("testnet-r0-c6", 6, slop_ninja_protocol::MAX_ROSTER as usize, None);
    let d = closed_roster(&s, &big, &artifacts_for(&big), time_of(118));
    let bytes = serde_json::to_vec(&d).unwrap();
    assert!(bytes.len() / d.rows.len() < contract::roster::ROW_BYTES);
    assert!(bytes.len() <= contract::bounds::ROSTER_STATUS);
    check(&Doc::Roster("a", "testnet-r0-c6".into()).key(), &bytes).unwrap();
    assert_eq!(contract::roster::ROWS, contract::uid_map::ROWS + slop_ninja_protocol::MAX_ROSTER as usize);
}

/// `/eval/status` at epoch `x` in engine phase `phase` for cohort 7, with
/// its per-hotkey admission rows and, when `frozen`, its freeze epoch.
fn eval_with_hotkeys(x: u64, phase: &str, log: &[(&str, u64)], frozen: Option<u64>, closing: bool) -> Value {
    let mut e = eval_json(x, phase, log, json!({}));
    if closing {
        e["calendar"]["current"]["window_open"] = json!(false);
        e["calendar"]["current"]["window_closing"] = json!(true);
        e["calendar"]["current"]["window_closed_epoch"] = json!(116);
    }
    e["admission"] = json!({"cohorts": [{"cohort": 7, "admitted": 1, "frozen_epoch": frozen,
        "sessions": [{"state": "verified", "count": 2}],
        "hotkeys": {"rule": crate::status::ADMISSION_HOTKEYS_RULE, "epoch": x, "block": block_of(x), "rows": [
            {"hotkey": hx(1), "uid": 3, "committed_block": null, "state": "none", "refusal_class": null},
            {"hotkey": hx(2), "uid": 1, "committed_block": block_of(101), "state": "committed", "refusal_class": null},
            {"hotkey": hx(3), "uid": 2, "committed_block": block_of(101), "state": "uploading", "refusal_class": null},
            {"hotkey": hx(4), "uid": 4, "committed_block": block_of(102), "state": if frozen.is_some() { "admitted" } else { "verified" }, "refusal_class": null,
             "roster_position": if frozen.is_some() { json!(1) } else { Value::Null }},
            {"hotkey": hx(5), "uid": 5, "committed_block": block_of(102), "state": "refused", "refusal_class": "artifact"},
            {"hotkey": hx(6), "uid": null, "committed_block": block_of(103), "state": if frozen.is_some() { "overflow" } else { "verifying" }, "refusal_class": null},
        ]}}]});
    e
}

fn statuses(d: &RosterDoc) -> Vec<(Option<u16>, &str, Option<&str>)> {
    d.rows.iter().map(|r| (r.uid, r.status.as_str(), r.reason_class.as_deref())).collect()
}

fn roster_at(level: RosterLevel, e: Value, x: u64, start: Option<&StartRecord>) -> Option<RosterDoc> {
    let c = chain();
    let mut s = settings(&c);
    s.roster = level;
    let o = observed(e);
    let mut memo = saved_at(x);
    rolled(&o, &mut memo, time_of(x));
    let now = now_of(&o, &memo);
    let d = running_roster(&s, level, &now, o.eval.as_ref(), start, time_of(x) + 3_600_000)?;
    let v = checked(&Doc::Roster("a", "testnet-r0-c7".into()), &d);
    assert_eq!(d.cohort.stage, now.cohort.stage, "the stage now.json shows");
    for r in v["rows"].as_array().unwrap() {
        assert!(r["result"].is_null() && r["roster_position"].is_null(), "no result before close");
    }
    Some(d)
}

#[test]
fn a_running_roster_shows_only_what_its_level_allows() {
    let open = || eval_with_hotkeys(110, "admission", &[("admission", 100)], None, false);
    assert_eq!(roster_at(RosterLevel::Closed, open(), 110, None), None, "the default writes no running roster");
    assert_eq!(roster_at(RosterLevel::Scoring, open(), 110, None), None, "nothing before scoring starts");
    // Frozen, before the freeze: commitments only.
    let d = roster_at(RosterLevel::Frozen, open(), 110, None).unwrap();
    assert_eq!((d.source.as_str(), d.level.as_str()), ("admission", "frozen"));
    assert_eq!(d.as_of, Some(AsOf { epoch: 110, block: Some(block_of(110)) }));
    assert_eq!(
        statuses(&d),
        [
            (Some(1), "committed", None),
            (Some(2), "committed", None),
            (Some(3), "none", None),
            (Some(4), "committed", None),
            (Some(5), "committed", None),
            (None, "committed", None)
        ]
    );
    // Sessions: the upload and verification states too.
    let d = roster_at(RosterLevel::Sessions, open(), 110, None).unwrap();
    assert_eq!(
        statuses(&d),
        [
            (Some(1), "committed", None),
            (Some(2), "uploading", None),
            (Some(3), "none", None),
            (Some(4), "verified", None),
            (Some(5), "refused", Some("artifact")),
            (None, "verifying", None)
        ]
    );
    // Frozen while the page still shows the window open: not yet shown.
    let d = roster_at(RosterLevel::Frozen, eval_with_hotkeys(110, "admission", &[("admission", 100)], Some(110), false), 110, None)
        .unwrap();
    assert!(statuses(&d).iter().all(|(_, s, _)| matches!(*s, "none" | "committed")));
    // From the freeze, at closing: the roster's outcome, at either level.
    for level in [RosterLevel::Frozen, RosterLevel::Sessions] {
        let d = roster_at(level, eval_with_hotkeys(116, "admission", &[("admission", 100)], Some(116), true), 116, None).unwrap();
        assert_eq!(d.cohort.stage, "admission_closing");
        assert_eq!(
            statuses(&d),
            [
                (Some(1), "committed", None),
                // An upload still open at the freeze is moot: committed.
                (Some(2), "committed", None),
                (Some(3), "none", None),
                (Some(4), "admitted", None),
                (Some(5), "refused", Some("artifact")),
                (None, "overflow", None)
            ],
            "{level:?}"
        );
    }
    // Building: admitted entrants are evaluating, with their artifact
    // state once the artifacts record exists.
    let start = StartRecord {
        schema: crate::artifacts::ARTIFACTS_SCHEMA.into(),
        cohort: 7,
        label: "testnet-r0-c7".into(),
        publisher: "test".into(),
        copy: "stream".into(),
        entrants: vec![StartEntry {
            uid: 4,
            hotkey: ss(4),
            commitment: "dd".repeat(32),
            state: ArtifactState::Published,
            header: Some("ee".repeat(32)),
            path: Some(format!("artifacts/v2/{}/header", "ee".repeat(32))),
            reason: Some("PRIVATE-DOCUMENT-TEXT".into()),
            files: Vec::new(),
        }],
    };
    let e = eval_with_hotkeys(118, "scoring", LOG_SCORING, Some(116), false);
    let d = roster_at(RosterLevel::Frozen, e.clone(), 118, Some(&start)).unwrap();
    let admitted = d.rows.iter().find(|r| r.uid == Some(4)).unwrap();
    assert_eq!((admitted.status.as_str(), admitted.artifact_state.as_deref()), ("evaluating", Some("published")));
    assert!(d.rows.iter().filter(|r| r.uid != Some(4)).all(|r| r.artifact_state.is_none()));
    // The scoring level alone: the artifacts record's entrants.
    let d = roster_at(RosterLevel::Scoring, e.clone(), 118, Some(&start)).unwrap();
    assert_eq!((d.source.as_str(), d.as_of.clone()), ("artifacts", None));
    assert_eq!(statuses(&d), [(Some(4), "evaluating", None)]);
    assert_eq!(d.rows[0].hotkey_hex.as_deref(), Some(hx(4).as_str()));
    let text = serde_json::to_string(&d).unwrap();
    for secret in ["PRIVATE-DOCUMENT-TEXT", "dddd", "eeee", "artifacts/v2"] {
        assert!(!text.contains(secret), "{secret}");
    }
    // Another cohort's record is never used.
    let mut other = start.clone();
    other.cohort = 6;
    assert_eq!(roster_at(RosterLevel::Scoring, e, 118, Some(&other)), None);
}

#[test]
fn below_the_sessions_level_a_session_without_a_commitment_reads_none() {
    // UID 2 has an open session but no logged commitment naming the cohort.
    let uncommitted = |frozen: Option<u64>, closing: bool, x: u64| {
        let mut e = eval_with_hotkeys(x, "admission", &[("admission", 100)], frozen, closing);
        e["admission"]["cohorts"][0]["hotkeys"]["rows"][2]["committed_block"] = Value::Null;
        e
    };
    let uid2 = |d: &RosterDoc| d.rows.iter().find(|r| r.uid == Some(2)).map(|r| r.status.clone()).unwrap();
    let d = roster_at(RosterLevel::Frozen, uncommitted(None, false, 110), 110, None).unwrap();
    assert_eq!(uid2(&d), "none", "the session is not chain data");
    let d = roster_at(RosterLevel::Frozen, uncommitted(Some(116), true, 116), 116, None).unwrap();
    assert_eq!(uid2(&d), "none", "nor after the freeze");
    let d = roster_at(RosterLevel::Sessions, uncommitted(None, false, 110), 110, None).unwrap();
    assert_eq!(uid2(&d), "uploading", "the sessions level shows it");
    let d = roster_at(RosterLevel::Sessions, uncommitted(Some(116), true, 116), 116, None).unwrap();
    assert_eq!(uid2(&d), "committed");
}

#[test]
fn admission_rows_are_read_whole_or_not_at_all() {
    let rows_of = |f: &dyn Fn(&mut Value)| {
        let mut e = eval_with_hotkeys(110, "admission", &[("admission", 100)], None, false);
        f(&mut e["admission"]["cohorts"][0]["hotkeys"]);
        let v = crate::status::parse_eval(&e);
        assert_eq!(v.admitted, [(7, 1)], "the rest of the status still parses");
        v.admission[0].hotkeys.as_ref().map(|h| h.rows.len())
    };
    assert_eq!(rows_of(&|_| {}), Some(6));
    assert_eq!(rows_of(&|h| h["rule"] = json!("v0")), None);
    assert_eq!(rows_of(&|h| h["rows"][0]["state"] = json!("scored")), None, "outside the closed list");
    assert_eq!(rows_of(&|h| h["rows"][4]["refusal_class"] = Value::Null), None, "refused without a class");
    assert_eq!(rows_of(&|h| h["rows"][4]["refusal_class"] = json!("object_mismatch")), None, "a code, not a class");
    assert_eq!(rows_of(&|h| h["rows"][0]["refusal_class"] = json!("artifact")), None, "a class without a refusal");
    assert_eq!(rows_of(&|h| h["rows"][1]["hotkey"] = json!(hx(1))), None, "a repeated hotkey");
    assert_eq!(rows_of(&|h| h["rows"][1]["uid"] = json!(3)), None, "a repeated UID");
    assert_eq!(rows_of(&|h| h["rows"] = json!("x")), None, "malformed, not fatal");
    assert_eq!(rows_of(&|h| *h = Value::Null), None);
    let over: Vec<Value> = (0..=contract::roster::ROWS as u16)
        .map(|u| json!({"hotkey": hex::encode([&[(u >> 8) as u8, u as u8][..], &[1; 30]].concat()), "uid": null,
                        "committed_block": null, "state": "none", "refusal_class": null}))
        .collect();
    assert_eq!(rows_of(&|h| h["rows"] = json!(over)), None, "more rows than the bound");
}

#[test]
fn roster_checks_refuse_what_the_contract_forbids() {
    let c = chain();
    let s = settings(&c);
    let sm = mixed_summary();
    let closed = value(&closed_roster(&s, &sm, &artifacts_for(&sm), time_of(118)));
    let running = value(
        &roster_at(
            RosterLevel::Frozen,
            eval_with_hotkeys(116, "admission", &[("admission", 100)], Some(116), true),
            116,
            None,
        )
        .unwrap(),
    );
    let refused = |base: &Value, label: &str, f: &dyn Fn(&mut Value)| {
        let mut x = base.clone();
        f(&mut x);
        check(&Doc::Roster("a", label.into()).key(), &serde_json::to_vec(&x).unwrap()).is_err()
    };
    let c6 = |f: &dyn Fn(&mut Value)| refused(&closed, "testnet-r0-c6", f);
    let c7 = |f: &dyn Fn(&mut Value)| refused(&running, "testnet-r0-c7", f);
    assert!(!c6(&|_| {}) && !c7(&|_| {}));
    assert!(c6(&|x| x["rows"][0]["q"] = json!("0.90")), "no score field");
    assert!(c6(&|x| x["rows"][0]["rank"] = json!(1)));
    assert!(c6(&|x| x["rows"].as_array_mut().unwrap().reverse()), "roster order is not UID order");
    assert!(c6(&|x| x["rows"][0]["status"] = json!("evaluating")));
    assert!(c6(&|x| x["rows"][0]["result"]["gate"] = json!("withheld")), "a gate that disagrees with the status");
    assert!(c6(&|x| x["rows"][2]["result"]["copy_gate"] = json!("pass")), "a withheld entrant with a copy bit");
    assert!(c6(&|x| x["rows"][0]["roster_position"] = Value::Null));
    assert!(c6(&|x| x["cohort"]["stage"] = json!("transition")), "a result before close");
    assert!(c6(&|x| x["references"]["current"]["content"] = json!("x")));
    assert!(c7(&|x| x["rows"][0]["result"] = json!({"gate": "pass", "copy_gate": "pass", "band": 1, "cluster": null})));
    assert!(c7(&|x| x["rows"][0]["roster_position"] = json!(1)));
    assert!(c7(&|x| x["rows"][0]["status"] = json!("scored")));
    assert!(c7(&|x| x["rows"][0]["status"] = json!("uploading")), "a session state below the sessions level");
    assert!(c7(&|x| x["rows"][4]["reason_class"] = Value::Null), "refused without a class");
    assert!(c7(&|x| x["rows"][4]["reason_class"] = json!("object_mismatch")));
    assert!(c7(&|x| x["rows"][0]["reason_class"] = json!("artifact")));
    assert!(c7(&|x| x["rows"][1]["hotkey"] = json!(ss(9))), "SS58 and hex disagree");
    assert!(c7(&|x| x["rows"][1]["uid"] = json!(1)), "a repeated UID");
    assert!(c7(&|x| x["level"] = json!("closed")));
    assert!(c7(&|x| x["source"] = json!("summary")));
    assert!(c7(&|x| x["references"] = json!({"index": 0, "current": {"content": hx(1), "artifact": hx(1)}, "predecessor": null})));
    assert!(c7(&|x| x["cohort"]["stage"] = json!("queued")));
    assert!(c7(&|x| x["sample"] = json!(true)));
}

/// The allow-list names exactly the fields of fully populated documents.
#[test]
fn the_roster_allow_list_is_exactly_the_contract() {
    let c = chain();
    let s = settings(&c);
    let sm = mixed_summary();
    let mut v = value(&closed_roster(&s, &sm, &artifacts_for(&sm), time_of(118)));
    v["as_of"] = json!({"epoch": 1, "block": 2});
    v["rows"][0]["reason_class"] = json!("artifact");
    v["rows"][0]["challenges"] = json!({"state": "running", "done": 1, "total": 2});
    let paths = crate::allowlist::paths(&v);
    assert_eq!(paths, allowed_paths(&Doc::Roster("a", "c".into())));
}

#[test]
fn a_run_writes_the_registry_and_the_rosters_at_their_levels() {
    let c = chain();
    let dash = MemStore::new();
    let public_bucket = public_with(&[(5, "testnet-r0-c5", Some(9)), (6, "testnet-r0-c6", None)]);
    let idx = index_with(&[(5, "testnet-r0-c5", Some(9), 0), (6, "testnet-r0-c6", None, 1)]);
    let out = DashboardStore::new(&dash);
    let public = PublicStore::new(&public_bucket);
    let budget = crate::arena::Budget::worker();
    let mut o = observed(eval_with_hotkeys(116, "admission", &[("admission", 100)], Some(116), true));
    o.coordinator = Some(
        coordinator_with(Some(registry_json(json!([{"uid": 1, "hotkey": hx(2), "registration_block": 10}])))),
    );
    let run = |memo: &mut Memo, level: RosterLevel| {
        let mut settings = settings(&c);
        settings.roster = level;
        let inputs = RunInputs {
            settings,
            observed: &o,
            index: Some(&idx),
            now_ms: time_of(116),
            budget: &budget,
        };
        block_on(publish(&out, &public, &inputs, memo, saved, &|| true))
    };
    let mut memo = saved_at(116);
    let pass = run(&mut memo, RosterLevel::Closed);
    assert!(pass.refused.is_empty(), "{:?}", pass.refused);
    for k in [
        "dashboard/v1/registry.json",
        "dashboard/v1/m/a/rosters/testnet-r0-c5.json",
        "dashboard/v1/m/a/rosters/testnet-r0-c6.json",
    ] {
        assert!(pass.written.contains(&k.to_owned()), "{k} in {:?}", pass.written);
        check(k, &dash.bytes(k).unwrap()).unwrap();
    }
    assert!(!pass.written.iter().any(|k| k.contains("rosters/testnet-r0-c7")), "closed: no running roster");
    let reg: RegistryDoc = serde_json::from_slice(&dash.bytes("dashboard/v1/registry.json").unwrap()).unwrap();
    assert_eq!(reg.rows.map(|r| r.len()), Some(1));
    // At the frozen level the running cohort's roster is written each run;
    // the closed rosters only when their inputs change.
    let pass = run(&mut memo, RosterLevel::Frozen);
    assert!(pass.refused.is_empty(), "{:?}", pass.refused);
    assert_eq!(
        pass.written,
        [
            "dashboard/v1/index.json",
            "dashboard/v1/m/a/now.json",
            "dashboard/v1/registry.json",
            "dashboard/v1/m/a/rosters/testnet-r0-c7.json",
            "dashboard/v1/m/a/cohorts/index.json"
        ]
    );
    let roster: RosterDoc = serde_json::from_slice(&dash.bytes("dashboard/v1/m/a/rosters/testnet-r0-c7.json").unwrap()).unwrap();
    assert_eq!(roster.cohort.stage, "admission_closing");
    assert!(roster.rows.iter().any(|r| r.status == "admitted"));
    // A coordinator without the field writes no registry.
    let mut o2 = o.clone();
    o2.coordinator = Some(parse_worker(&worker_status("primary", false)));
    let inputs = RunInputs {
        settings: settings(&c),
        observed: &o2,
        index: Some(&idx),
        now_ms: time_of(116),
        budget: &budget,
    };
    let pass = block_on(publish(&out, &public, &inputs, &mut memo, saved, &|| true));
    assert!(!pass.written.contains(&"dashboard/v1/registry.json".to_owned()));
    assert_eq!(budget.held(), 0, "every reservation released");
}

/// Every key name anywhere in `v`.
fn keys(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                out.insert(k.clone());
                keys(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
        _ => {}
    }
}

/// The approved testnet level (`sessions`) publishes each miner's intake
/// while the cohort runs, at every running stage, and nothing score-like:
/// no score, rank, roster position, gate, band, cluster or cutoff, even
/// when the eval Worker's private rows carry such fields.
#[test]
fn the_sessions_level_shows_intake_and_nothing_score_like_before_close() {
    let mut late = LOG_SCORING.to_vec();
    late.extend([("packaging", 119), ("transition", 121)]);
    /// `(epoch, engine phase, phase log, freeze epoch, closing, bar stage)`.
    type Case<'a> = (u64, &'a str, &'a [(&'a str, u64)], Option<u64>, bool, &'a str);
    let cases: [Case<'_>; 5] = [
        (110, "admission", &[("admission", 100)], None, false, "admission_open"),
        (116, "admission", &[("admission", 100)], Some(116), true, "admission_closing"),
        (118, "scoring", LOG_SCORING, Some(116), false, "scoring"),
        (120, "packaging", &late[..6], Some(116), false, "packaging"),
        (121, "transition", &late, Some(116), false, "transition"),
    ];
    let forbidden = [
        "score", "q", "rank", "band", "gate", "copy_gate", "cluster", "cutoff", "eligible", "row_share_pct",
        "best_score", "quality", "bands", "promoted", "session", "digest", "refusal",
    ];
    for (x, phase, log, frozen, closing, stage) in cases {
        let mut e = eval_with_hotkeys(x, phase, log, frozen, closing);
        e["cohorts"][1]["reward_v1"] = json!({"published_epoch": if phase == "transition" { json!(121) } else { Value::Null }});
        // Private fields a future eval build might add: never published.
        for r in e["admission"]["cohorts"][0]["hotkeys"]["rows"].as_array_mut().unwrap() {
            r["score"] = json!("0.912345");
            r["q"] = json!("0.91");
            r["band"] = json!(1);
            r["gate"] = json!("pass");
            r["session"] = json!("PLANTED-SESSION-ID");
            r["refusal"] = json!({"code": "object_mismatch", "message": "PLANTED-DETAIL"});
        }
        e["admission"]["cohorts"][0]["progress"] = json!({"rule": crate::status::SCORING_PROGRESS_RULE, "cohort": 7,
            "sealed": x >= 118, "rows": if x >= 118 { json!([{"hotkey": hx(4), "started": true, "done": 7, "total": 9,
                "score": "0.912345", "q": "0.91", "gate": "pass", "session": "PLANTED-SESSION-ID"}]) } else { json!([]) }});
        let d = roster_at(RosterLevel::Sessions, e, x, None).unwrap_or_else(|| panic!("{stage}: a roster"));
        assert_eq!((d.cohort.stage.as_str(), d.level.as_str(), d.source.as_str()), (stage, "sessions", "admission"));
        let v = value(&d);
        let mut seen = BTreeSet::new();
        keys(&v, &mut seen);
        for k in forbidden {
            assert!(!seen.contains(k), "{stage}: key {k:?} published");
        }
        let text = v.to_string();
        for planted in ["0.912345", "0.91", "PLANTED", "object_mismatch"] {
            assert!(!text.contains(planted), "{stage}: {planted} published");
        }
        for r in &d.rows {
            assert!(r.result.is_none() && r.roster_position.is_none(), "{stage}: a result or position before close");
            assert!(!["scored", "withheld", "not_executed"].contains(&r.status.as_str()), "{stage}");
        }
        // Intake is visible: before the freeze the session states, from it
        // the roster's outcome (evaluating once work starts).
        let st: BTreeSet<&str> = d.rows.iter().map(|r| r.status.as_str()).collect();
        let expect: &[&str] = match stage {
            "admission_open" => &["committed", "none", "uploading", "verified", "refused", "verifying"],
            "admission_closing" => &["committed", "none", "admitted", "refused", "overflow"],
            _ => &["committed", "none", "evaluating", "refused", "overflow"],
        };
        assert_eq!(st, expect.iter().copied().collect(), "{stage}");
    }
}

/// The golden documents the website's Phase B types and the status API's
/// tests read (`tests/fixtures/dashboard-web/`): built here, they pass the
/// publisher's own check and parse back unchanged. Set
/// `SLOP_NINJA_UPDATE_GOLDEN=1` to rewrite them after a deliberate change.
#[test]
fn the_golden_registry_and_roster_documents_are_current() {
    let c = chain();
    let s = settings(&c);
    let sm = mixed_summary();
    let registry = registry_doc(
        &s,
        Some(&RegistryView {
            epoch: 116,
            block: block_of(116) + 40,
            rows: vec![(1, [2; 32], 10), (2, [3; 32], 11), (3, [1; 32], 12), (4, [4; 32], 13), (5, [5; 32], 14)],
        }),
        time_of(116) + 3_600_000,
    );
    let closing = roster_at(
        RosterLevel::Frozen,
        eval_with_hotkeys(116, "admission", &[("admission", 100)], Some(116), true),
        116,
        None,
    )
    .unwrap();
    // The approved testnet level while the window admits: intake states.
    let open = roster_at(
        RosterLevel::Sessions,
        eval_with_hotkeys(110, "admission", &[("admission", 100)], None, false),
        110,
        None,
    )
    .unwrap();
    // The approved level while scoring runs: challenge counts.
    let scoring = roster_at(
        RosterLevel::Sessions,
        eval_with_progress(118, "scoring", LOG_SCORING, Some(&[(4, true, 142, 312)])),
        118,
        None,
    )
    .unwrap();
    let docs: [(&str, Doc, Value); 5] = [
        ("registry", Doc::Registry, value(&registry)),
        ("roster-open-testnet-r0-c7", Doc::Roster("a", "testnet-r0-c7".into()), value(&open)),
        ("roster-scoring-testnet-r0-c7", Doc::Roster("a", "testnet-r0-c7".into()), value(&scoring)),
        ("roster-closing-testnet-r0-c7", Doc::Roster("a", "testnet-r0-c7".into()), value(&closing)),
        (
            "roster-closed-testnet-r0-c6",
            Doc::Roster("a", "testnet-r0-c6".into()),
            value(&closed_roster(&s, &sm, &artifacts_for(&sm), time_of(118))),
        ),
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dashboard-web");
    for (name, doc, v) in docs {
        let text = serde_json::to_string_pretty(&v).unwrap() + "\n";
        check(&doc.key(), text.as_bytes()).unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = dir.join(format!("{name}.json"));
        if std::env::var_os("SLOP_NINJA_UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let stored = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(stored, text, "{name} is stale: rerun with SLOP_NINJA_UPDATE_GOLDEN=1");
        match doc {
            Doc::Registry => assert_eq!(value(&serde_json::from_str::<RegistryDoc>(&stored).unwrap()), v),
            _ => assert_eq!(value(&serde_json::from_str::<RosterDoc>(&stored).unwrap()), v),
        }
    }
}

/// [`eval_with_hotkeys`] with the cohort's `progress`: `rows` as
/// `(hotkey byte, started, done, total)`, `None` for the pre-seal report.
fn eval_with_progress(x: u64, phase: &str, log: &[(&str, u64)], rows: Option<&[(u8, bool, u64, u64)]>) -> Value {
    let mut e = eval_with_hotkeys(x, phase, log, Some(116), x == 116);
    let list: Vec<Value> = rows
        .unwrap_or_default()
        .iter()
        .map(|(k, started, done, total)| json!({"hotkey": hx(*k), "started": started, "done": done, "total": total}))
        .collect();
    e["admission"]["cohorts"][0]["progress"] =
        json!({"rule": crate::status::SCORING_PROGRESS_RULE, "cohort": 7, "sealed": rows.is_some(), "rows": list});
    e
}

fn challenges_of(d: &RosterDoc) -> Vec<(Option<u16>, Option<Challenges>)> {
    d.rows.iter().map(|r| (r.uid, r.challenges.clone())).collect()
}

fn ch(state: &str, done: u32, total: Option<u32>) -> Option<Challenges> {
    Some(Challenges { state: state.into(), done, total })
}

/// The sessions level shows each admitted entrant's challenge counts:
/// pending before the seal, then done of total, on the entrant's row only;
/// lower levels show none; a refused or other cohort's report shows none.
#[test]
fn the_sessions_level_shows_each_entrants_challenge_counts_only() {
    // Before the seal (the roster just froze): pending, no total yet.
    let closing = eval_with_progress(116, "admission", &[("admission", 100)], None);
    let d = roster_at(RosterLevel::Sessions, closing.clone(), 116, None).unwrap();
    let pending = ch("pending", 0, None);
    assert_eq!(challenges_of(&d), [(Some(1), None), (Some(2), None), (Some(3), None), (Some(4), pending), (Some(5), None), (None, None)]);
    for level in [RosterLevel::Frozen, RosterLevel::Scoring] {
        if let Some(d) = roster_at(level, closing.clone(), 116, None) {
            assert!(d.rows.iter().all(|r| r.challenges.is_none()), "{level:?}");
        }
    }
    // Scoring: done of total; a sealed report without the entrant's row (an
    // entrant the seal does not execute) gives none.
    let scoring = |rows: &[(u8, bool, u64, u64)]| eval_with_progress(118, "scoring", LOG_SCORING, Some(rows));
    let uid4 = |e: Value, level: RosterLevel| {
        roster_at(level, e, 118, None).and_then(|d| d.rows.iter().find(|r| r.uid == Some(4)).and_then(|r| r.challenges.clone()))
    };
    assert_eq!(uid4(scoring(&[(4, true, 142, 312)]), RosterLevel::Sessions), ch("running", 142, Some(312)));
    assert_eq!(uid4(scoring(&[(4, false, 0, 312)]), RosterLevel::Sessions), ch("pending", 0, Some(312)));
    assert_eq!(uid4(scoring(&[(4, true, 0, 312)]), RosterLevel::Sessions), ch("running", 0, Some(312)), "started");
    assert_eq!(uid4(scoring(&[(4, true, 312, 312)]), RosterLevel::Sessions), ch("complete", 312, Some(312)));
    assert_eq!(uid4(scoring(&[(9, true, 3, 312)]), RosterLevel::Sessions), None, "not executed: no row");
    assert_eq!(uid4(scoring(&[(4, true, 142, 312)]), RosterLevel::Frozen), None, "below the sessions level");
    // Only on the entrant's row, even when the report names other hotkeys.
    let d = roster_at(RosterLevel::Sessions, scoring(&[(1, true, 5, 9), (4, true, 142, 312), (6, true, 2, 9)]), 118, None).unwrap();
    assert_eq!(d.rows.iter().filter(|r| r.challenges.is_some()).map(|r| r.uid).collect::<Vec<_>>(), [Some(4)]);
    // A report that is not whole, or names another cohort: none at all.
    let bads: [&dyn Fn(&mut Value); 10] = [
        &|p: &mut Value| p["rule"] = json!("slop_ninja.eval.scoring_progress/v0"),
        &|p: &mut Value| p["cohort"] = json!(6),
        &|p: &mut Value| p["rows"][0]["done"] = json!(313),
        &|p: &mut Value| p["rows"][0]["total"] = json!(0),
        &|p: &mut Value| p["rows"][0]["started"] = json!(false),
        &|p: &mut Value| p["rows"] = Value::Null,
        &|p: &mut Value| p["sealed"] = json!(false),
        &|p: &mut Value| p["rows"][0]["hotkey"] = json!(hx(0xab).to_uppercase()),
        &|p: &mut Value| p["rows"] = json!([p["rows"][0].clone(), p["rows"][0].clone()]),
        &|p: &mut Value| *p = json!({"refused": "over_bound", "rule": crate::status::SCORING_PROGRESS_RULE, "cohort": 7, "rows": null}),
    ];
    for bad in bads {
        let mut e = scoring(&[(4, true, 142, 312)]);
        bad(&mut e["admission"]["cohorts"][0]["progress"]);
        assert_eq!(crate::status::parse_eval(&e).admission[0].progress, None);
        assert_eq!(uid4(e, RosterLevel::Sessions), None);
    }
    // The artifacts record path (no per-hotkey rows) joins the counts too.
    let mut e = scoring(&[(4, true, 142, 312)]);
    e["admission"]["cohorts"][0]["hotkeys"] = Value::Null;
    let start = StartRecord {
        schema: crate::artifacts::ARTIFACTS_SCHEMA.into(),
        cohort: 7,
        label: "testnet-r0-c7".into(),
        publisher: "test".into(),
        copy: "stream".into(),
        entrants: vec![StartEntry {
            uid: 4,
            hotkey: ss(4),
            commitment: "dd".repeat(32),
            state: ArtifactState::Published,
            header: None,
            path: None,
            reason: None,
            files: Vec::new(),
        }],
    };
    let d = roster_at(RosterLevel::Sessions, e, 118, Some(&start)).unwrap();
    assert_eq!((d.source.as_str(), challenges_of(&d)), ("artifacts", vec![(Some(4), ch("running", 142, Some(312)))]));
}

/// Nothing of the eval Worker's progress rows but the counts is published,
/// even when they carry score, outcome, fault or retry fields.
#[test]
fn challenge_counts_publish_nothing_but_counts() {
    let mut e = eval_with_progress(118, "scoring", LOG_SCORING, Some(&[(4, true, 142, 312)]));
    let p = &mut e["admission"]["cohorts"][0]["progress"];
    p["leader"] = json!(hx(4));
    p["rows"][0]["score"] = json!("0.912345");
    p["rows"][0]["utility"] = json!("0.7777");
    p["rows"][0]["failures"] = json!(3);
    p["rows"][0]["retries"] = json!(5);
    p["rows"][0]["attempt"] = json!(4);
    p["rows"][0]["fault"] = json!("PLANTED-FAULT");
    p["rows"][0]["tasks"] = json!([17, 18, 19]);
    p["rows"][0]["job"] = json!(2);
    p["rows"][0]["chunk"] = json!(11);
    let d = roster_at(RosterLevel::Sessions, e, 118, None).unwrap();
    let v = value(&d);
    let mut seen = BTreeSet::new();
    keys(&v, &mut seen);
    for k in ["score", "utility", "failures", "retries", "attempt", "fault", "tasks", "job", "chunk", "leader", "started"] {
        assert!(!seen.contains(k), "key {k:?} published");
    }
    let text = v.to_string();
    for planted in ["0.912345", "0.7777", "PLANTED", "[17,18,19]"] {
        assert!(!text.contains(planted), "{planted} published");
    }
    let row = v["rows"].as_array().unwrap().iter().find(|r| r["uid"] == 4).unwrap();
    assert_eq!(row["challenges"], json!({"state": "running", "done": 142, "total": 312}));
}

#[test]
fn roster_checks_refuse_challenge_counts_the_contract_forbids() {
    let running = value(&roster_at(RosterLevel::Sessions, eval_with_progress(118, "scoring", LOG_SCORING, Some(&[(4, true, 142, 312)])), 118, None).unwrap());
    let i4 = running["rows"].as_array().unwrap().iter().position(|r| r["uid"] == 4).unwrap();
    let refused = |f: &dyn Fn(&mut Value)| {
        let mut x = running.clone();
        f(&mut x);
        check(&Doc::Roster("a", "testnet-r0-c7".into()).key(), &serde_json::to_vec(&x).unwrap()).is_err()
    };
    let counts = |state: &str, done: u64, total: Value| json!({"state": state, "done": done, "total": total});
    assert!(!refused(&|_| {}));
    assert!(!refused(&|x| x["rows"][i4]["challenges"] = counts("complete", 312, json!(312))));
    assert!(!refused(&|x| x["rows"][i4]["challenges"] = counts("pending", 0, Value::Null)));
    assert!(refused(&|x| x["rows"][i4]["challenges"] = counts("running", 313, json!(312))), "more done than total");
    assert!(refused(&|x| x["rows"][i4]["challenges"] = counts("complete", 311, json!(312))));
    assert!(refused(&|x| x["rows"][i4]["challenges"] = counts("pending", 3, json!(312))));
    assert!(refused(&|x| x["rows"][i4]["challenges"] = counts("running", 3, Value::Null)));
    assert!(refused(&|x| x["rows"][i4]["challenges"] = counts("failed", 3, json!(312))), "no failure state");
    assert!(refused(&|x| x["rows"][i4]["challenges"]["score"] = json!("0.9")), "no other field");
    assert!(refused(&|x| x["rows"][i4]["challenges"]["retries"] = json!(1)));
    assert!(refused(&|x| x["rows"][0]["challenges"] = counts("running", 3, json!(312))), "on a non-entrant row");
    assert!(refused(&|x| x["level"] = json!("frozen")), "below the sessions level");
}

/// A row with every field at its longest, the challenge counts included,
/// stays under the contract's row bound.
#[test]
fn the_longest_roster_row_with_counts_fits_the_row_bound() {
    let row = RosterRow {
        uid: Some(u16::MAX),
        hotkey: crate::ss58::encode(&AccountId([0xff; 32])),
        hotkey_hex: Some("f".repeat(64)),
        status: "not_executed".into(),
        reason_class: Some("registration".into()),
        roster_position: Some(u32::MAX),
        result: Some(RowResult {
            gate: "not_executed".into(),
            copy_gate: Some("pass".into()),
            band: Some(u32::MAX),
            cluster: Some(ClusterRef { uid: u16::MAX, hotkey: crate::ss58::encode(&AccountId([0xff; 32])) }),
        }),
        artifact_state: Some("published".into()),
        challenges: Some(Challenges { state: "complete".into(), done: u32::MAX, total: Some(u32::MAX) }),
    };
    let bytes = serde_json::to_vec_pretty(&row).unwrap().len() + 2;
    assert!(bytes < contract::roster::ROW_BYTES, "{bytes}");
    assert_eq!(contract::roster::ENTRANTS, slop_ninja_protocol::MAX_ROSTER as usize);
}

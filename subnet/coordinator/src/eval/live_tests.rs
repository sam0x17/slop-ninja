//! Live values while a cohort is scored (`eval::live`): the cadence of the
//! releases, the privacy accounting (batch only, release once, nothing
//! exact in the report, the bit bound), the operator stop, a failed setup
//! that holds nothing, the operator's clear of every live fault, and the
//! package cross-check.

use super::*;
use crate::eval::live;
use crate::eval::plan::LivePlan;

const LIVE_LABEL: &str = "testnet-rv1-live";
/// A fine live grid at this small batch, so the negative controls see any
/// change in a segment's mean.
const EPS_LIVE_MILLI: u32 = 1_000_000;

fn live_block() -> LivePlan {
    LivePlan { rule: "live-v1".into(), prefinal: true, eps_live_milli: EPS_LIVE_MILLI, segments_max: 4 }
}

/// A seeded reward-v1 cohort of 40 batch documents (segments of 10, chunks
/// of 4) with three entrants: a copy of R_0's predictions (job 1), a
/// stronger model (job 2), and one whose container crashes on every request
/// (job 3: every document a model error, so withheld). Calls `f` with the
/// cohort open.
fn with_live_cohort<T>(live: Option<LivePlan>, f: impl FnOnce(&Sqlite, &mut Sim<'_>, i64) -> T) -> T {
    let db = Sqlite::open();
    let (mut sim, weak, probe_ref) = probe_built_doc(&db, Some(crate::eval::plan::TASK_ORDER_SEEDED_RANK));
    let id = crate::eval::plan::tests::identity;
    let copy = v3_artifact(&mut sim, id(31, 0xd1), [1, 2, 3, 31, 5]);
    let good = v3_artifact(&mut sim, id(32, 0xd2), [1, 2, 3, 32, 5]);
    let crash = v3_artifact(&mut sim, id(33, 0xd3), [1, 2, 3, 33, 5]);
    let mut p = plan_v3(2, LIVE_LABEL, 40, weak);
    p.entrants = vec![copy, good, crash];
    p.allow_list = vec![id(31, 0xd1), id(32, 0xd2), id(33, 0xd3)];
    {
        let v = p.reward_v1.as_mut().unwrap();
        v.probes = vec![probe_ref];
        v.canary.draw_humans = 8;
        v.task_order = Some(crate::eval::plan::TASK_ORDER_SEEDED_RANK.into());
        v.live = live;
    }
    sim.predict_doc = Some(Box::new(|job, kind, sha| {
        let c = match job {
            0 | 1 => doc_conf(1, kind, sha, (35.0, 65.0, 70.0), 10.0),
            _ => doc_conf(4, kind, sha, (3.0, 95.0, 97.0), 7.0),
        };
        (c, c * 0.4)
    }));
    sim.score = Box::new(|q| if q.job == 3 { Score::Crash } else { Score::Ok });
    let seq = open_cohort(&db, &p, 20, sim.now);
    f(&db, &mut sim, seq)
}

fn q1(db: &Sqlite, sql: &str, seq: i64) -> i64 {
    db.conn.query_row(sql, [seq], |r| r.get(0)).unwrap()
}

/// The leading segments of `job` whose batch tasks all have an outcome.
fn resolved_segments(db: &Sqlite, seq: i64, job: i64, segments: i64) -> i64 {
    let mut k = 0;
    for sg in 1..=segments {
        let open: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM docs d WHERE d.seq = ?1 AND d.live_segment = ?2
                   AND NOT EXISTS (SELECT 1 FROM preds p WHERE p.seq = d.seq AND p.job = ?3 AND p.task = d.task)",
                rusqlite::params![seq, sg, job],
                |r| r.get(0),
            )
            .unwrap();
        if open > 0 {
            break;
        }
        k += 1;
    }
    k
}

fn job_done(db: &Sqlite, seq: i64, job: i64) -> bool {
    let (tasks, preds): (i64, i64) = db
        .conn
        .query_row(
            "SELECT tasks, (SELECT COUNT(*) FROM preds p WHERE p.seq = j.seq AND p.job = j.job) FROM jobs j WHERE seq = ?1 AND job = ?2",
            rusqlite::params![seq, job],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    preds == tasks
}

fn releases(db: &Sqlite, seq: i64) -> Vec<(i64, String, i64, Option<String>, String, i64)> {
    db.conn
        .prepare("SELECT job, kind, checkpoint, value_units, outcome, revision FROM live_releases WHERE seq = ?1 ORDER BY job, kind, checkpoint, revision")
        .unwrap()
        .query_map([seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn report(db: &Sqlite) -> Value {
    tx(db, live::report).unwrap()
}

/// The entrant row of the report for owner byte `b` (hotkey `[b; 32]`).
fn row(r: &Value, b: u8) -> &Value {
    let hk = hex::encode([b; 32]);
    r["rows"].as_array().unwrap().iter().find(|x| x["hotkey"] == hk.as_str()).unwrap()
}

/// Ticks until the cohort reaches `transition`, checking the cadence at
/// every tick: the live step runs first in each alarm step, so after tick T
/// each subject has exactly one reading per leading segment it had resolved
/// before T (at most L - 1), and a pre-finalized value exactly when its job
/// and R_k's were done before T. Shown values never change. Returns every
/// report seen.
fn run_watching(db: &Sqlite, sim: &mut Sim<'_>, seq: i64, max: usize) -> Vec<Value> {
    let mut seen = Vec::new();
    let mut shown: BTreeMap<(i64, String, i64), (Option<String>, String)> = BTreeMap::new();
    for _ in 0..max {
        let segs = db
            .conn
            .query_row("SELECT segments FROM live_cohorts WHERE seq = ?1", [seq], |r| r.get::<_, i64>(0))
            .ok();
        let jobs: Vec<(i64, String)> = db
            .conn
            .prepare("SELECT job, role FROM jobs WHERE seq = ?1 ORDER BY job")
            .unwrap()
            .query_map([seq], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let before: Vec<(i64, String, i64, bool)> = jobs
            .iter()
            .map(|(j, role)| (*j, role.clone(), segs.map_or(0, |l| resolved_segments(db, seq, *j, l)), job_done(db, seq, *j)))
            .collect();
        let ref_done = before.iter().any(|(_, role, _, done)| role == "reference" && *done);
        let halted = q1(db, "SELECT COUNT(*) FROM live_stop WHERE stopped = 1 AND ?1 = ?1", seq) > 0;
        let was_running = matches!(phase(db, seq).as_str(), "scoring" | "packaging");
        sim.tick();
        let r = report(db);
        assert!(r.get("refused").is_none(), "{r}");
        if let Some(l) = segs.filter(|_| was_running) {
            for (job, role, k, done) in &before {
                if role == "previous_reference" {
                    continue;
                }
                let got = q1(db, &format!("SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND job = {job} AND kind = 'reading'"), seq);
                let want = if halted { 0 } else { (*k).min(l - 1).max(0) };
                assert_eq!(got, want, "job {job} readings after resolving {k} segments (stopped {halted})");
                let pf = q1(db, &format!("SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND job = {job} AND kind = 'prefinal'"), seq);
                assert_eq!(pf == 1, *done && ref_done && !halted, "job {job} prefinal (done {done}, reference done {ref_done})");
            }
        }
        for (job, kind, cp, v, o, rev) in releases(db, seq) {
            assert_eq!(rev, 0, "no recomputation");
            let prev = shown.insert((job, kind.clone(), cp), (v.clone(), o.clone()));
            assert!(prev.is_none_or(|p| p == (v, o)), "a shown value changed");
        }
        seen.push(r);
        if phase(db, seq) == "transition" {
            return seen;
        }
    }
    panic!("cohort {seq} did not reach transition: {}", phase(db, seq));
}

#[test]
fn live_values_follow_the_cohort_at_block_cadence_and_match_the_package() {
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        let seen = run_watching(db, sim, seq, 3_000);
        // Nothing before the seal.
        assert!(seen[0]["policy"].is_null() || seen[0]["rows"].as_array().unwrap().is_empty(), "{}", seen[0]);
        let r = seen.last().unwrap();
        let p = &r["policy"];
        assert_eq!((p["rule"].as_str(), p["segments"].as_u64(), p["released_max"].as_u64()), (Some("live-v1"), Some(4), Some(3)));
        assert_eq!(p["stopped"], false);
        let bits = &p["bits"];
        assert!(bits["counted"].as_u64().unwrap() + bits["live"].as_u64().unwrap() <= bits["bound"].as_u64().unwrap());
        // Batch only: exactly the 40 batch tasks carry a segment, 10 each.
        assert_eq!(q1(db, "SELECT COUNT(*) FROM docs WHERE seq = ?1 AND live_segment IS NOT NULL", seq), 40);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM docs WHERE seq = ?1 AND live_segment IS NOT NULL AND docset != 'batch'", seq), 0);
        for sg in 1..=4 {
            assert_eq!(q1(db, &format!("SELECT COUNT(*) FROM docs WHERE seq = ?1 AND live_segment = {sg}"), seq), 10);
        }
        // Three readings each for the copy, the strong model and R_k; the
        // crashing entrant is withheld from its first checkpoint.
        for b in [0xd1, 0xd2] {
            let cps = row(r, b)["checkpoints"].as_array().unwrap();
            assert_eq!(cps.len(), 3);
            assert!(cps.iter().all(|c| c.as_str().unwrap().starts_with("0.") || c == "1"), "{cps:?}");
            assert_eq!(row(r, b)["stopped"], false);
        }
        assert_eq!(row(r, 0xd3)["checkpoints"], json!(["withheld", "withheld", "withheld"]));
        assert_eq!(row(r, 0xd3)["stopped"], true);
        assert_eq!(row(r, 0xd3)["prefinal"], "withheld");
        assert_eq!(r["reference"]["checkpoints"].as_array().unwrap().len(), 3);
        assert_eq!((r["reference"]["done"].as_u64(), r["reference"]["total"].as_u64()), (Some(40), Some(40)));
        // Pre-finalized values are the package's own.
        let pkg = sim.packages_v3.last().unwrap();
        let pw = pkg.package.quant.width;
        let g = |v: u64| {
            let mut buf = [0u8; slop_ninja_protocol::quant::MAX_DECIMAL_LEN];
            slop_ninja_protocol::quant::format_grid(v, pw, &mut buf).unwrap().to_owned()
        };
        assert_eq!(r["prefinal"]["current"], g(pkg.package.quant.reference_q.get()));
        assert_eq!(r["prefinal"]["quantization"]["width"], g(pw));
        for (k, b) in [(0usize, 0xd1u8), (1, 0xd2)] {
            assert_eq!(row(r, b)["prefinal"], g(pkg.package.entrants[k].q.unwrap().get()), "entrant {k}");
        }
        assert!(pkg.live_defects.is_empty());
        assert_eq!(r["defects"], json!([]));
        // The margin line is on the grid at or above R_k's value.
        let m = r["prefinal"]["margin_line"].as_str().unwrap();
        assert!(slop_ninja_protocol::quant::parse_grid(m, pw).unwrap() >= pkg.package.quant.reference_q.get());
        // Nothing exact leaves: no exact score, SE or sum in any report.
        let text: String = seen.iter().map(Value::to_string).collect();
        let mut exact: Vec<u64> = pkg.package.entrants.iter().filter_map(|e| e.score.map(|s| s.get())).collect();
        exact.push(pkg.package.reference.score.get());
        exact.push(pkg.package.quant.se);
        for v in exact.into_iter().filter(|v| *v > 1_000_000) {
            for needle in [v.to_string(), format!("{v:x}"), hex::encode(v.to_be_bytes()), hex::encode(v.to_le_bytes())] {
                assert!(!text.contains(&needle), "exact value {v} in a report");
            }
        }
        for forbidden in ["rank", "band", "gate", "cluster", "eligible", "\"score\"", "_ms", "epoch"] {
            assert!(!text.contains(forbidden), "{forbidden} in a report");
        }
        // Release once: another pass releases nothing and changes nothing.
        let before = releases(db, seq);
        db.conn.execute("UPDATE live_cohorts SET checked_preds = -1 WHERE seq = ?1", [seq]).unwrap();
        assert!(!tx(db, |s| live::release_due(s, &cfg(), seq)).unwrap());
        assert_eq!(releases(db, seq), before);
        run_to_close(sim, db, seq, 22);
        // A closed cohort's live values leave the report.
        assert_eq!(report(db)["cohort"], Value::Null);
    });
}

/// Readings and pre-finalized values read batch tasks only: changing every
/// probe and audit prediction leaves them unchanged. (N) A segment that
/// holds probe tasks changes them, so the check would catch it.
#[test]
fn live_values_read_the_batch_only() {
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        run_watching(db, sim, seq, 3_000);
        let first = releases(db, seq);
        assert!(first.len() >= 10);
        let reset = |db: &Sqlite| {
            db.conn.execute("DELETE FROM live_releases WHERE seq = ?1", [seq]).unwrap();
            db.conn.execute("UPDATE live_cohorts SET checked_preds = -1, quant_width = NULL, margin_line = NULL WHERE seq = ?1", [seq]).unwrap();
            while tx(db, |s| live::release_due(s, &cfg(), seq)).unwrap() {}
        };
        db.conn
            .execute(
                "UPDATE preds SET inv_ppb = 1000000000 - inv_ppb WHERE seq = ?1 AND inv_ppb IS NOT NULL
                   AND task IN (SELECT task FROM docs WHERE seq = ?1 AND docset != 'batch' AND task IS NOT NULL)",
                [seq],
            )
            .unwrap();
        reset(db);
        assert_eq!(releases(db, seq), first, "probe and audit predictions never enter a live value");
        // Negative control: probe tasks inside segment 1.
        db.conn.execute("UPDATE docs SET live_segment = 1 WHERE seq = ?1 AND docset = 'probe' AND task IS NOT NULL", [seq]).unwrap();
        reset(db);
        assert_ne!(releases(db, seq), first, "a reading over probe slots must differ");
    });
}

/// The operator's stop: nothing is released while it holds, the report
/// shows the stop and no value, scoring and packaging run on, every change
/// is journaled, and a resume releases each due value once, in order.
#[test]
fn the_live_stop_holds_releases_only_and_resumes_in_order() {
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        let stop = |cohort: Option<u64>, stopped: bool| {
            tx(db, |s| live::set_stop(s, &live::StopRequest { stopped, cohort, reason: "operator drill".into(), clear_fault: false }))
                .unwrap()
        };
        // Refused bodies: an unknown cohort, an empty reason.
        let req = |stopped, cohort, reason: &str| live::StopRequest { stopped, cohort, reason: reason.into(), clear_fault: false };
        assert!(tx(db, |s| live::set_stop(s, &req(true, Some(99), "x"))).is_err());
        assert!(tx(db, |s| live::set_stop(s, &req(true, None, " "))).is_err());
        stop(None, true);
        stop(Some(2), true);
        let seen = run_watching(db, sim, seq, 3_000);
        assert_eq!(releases(db, seq), Vec::new(), "nothing released during the stop");
        let r = seen.last().unwrap();
        assert_eq!(r["policy"]["stopped"], true);
        assert!(r["prefinal"].is_null());
        assert_eq!(r["reference"]["checkpoints"], json!([]));
        assert!(r["rows"].as_array().unwrap().iter().all(|x| x["checkpoints"] == json!([]) && x["prefinal"].is_null()));
        assert_eq!(phase(db, seq), "transition", "scoring and packaging ran on");
        // Resuming the global scope alone still leaves the cohort's stop.
        stop(None, false);
        sim.tick();
        assert_eq!(releases(db, seq), Vec::new());
        stop(Some(2), false);
        sim.tick();
        let rel = releases(db, seq);
        for job in [0i64, 1, 2, 3] {
            let cps: Vec<i64> = rel.iter().filter(|x| x.0 == job && x.1 == "reading").map(|x| x.2).collect();
            assert_eq!(cps, [1, 2, 3], "job {job}: each checkpoint once, in order");
        }
        assert_eq!(rel.iter().filter(|x| x.1 == "prefinal").count(), 4);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE ?1 = ?1", seq), 4);
        assert_eq!(report(db)["policy"]["stopped"], false);
    });
}

/// A failed setup never holds the cohort: it seals, scores, packages and
/// closes; the setup stores no segments with its fault, so no reading is
/// released, and the pre-finalized values, which need no partition, still
/// appear.
#[test]
fn a_failed_live_setup_holds_nothing() {
    live::FAIL_PREPARE.with(|f| f.set(true));
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        let seen = run_watching(db, sim, seq, 3_000);
        live::FAIL_PREPARE.with(|f| f.set(false));
        let (segments, fault): (i64, Option<String>) =
            db.conn.query_row("SELECT segments, fault FROM live_cohorts WHERE seq = ?1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((segments, fault.as_deref()), (0, Some(live::FAULT_PREPARE)));
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND kind = 'reading'", seq), 0);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND kind = 'prefinal'", seq), 4);
        let r = seen.last().unwrap();
        assert_eq!(r["fault"], live::FAULT_PREPARE);
        assert!(!r["prefinal"].is_null());
        run_to_close(sim, db, seq, 22);
    });
    live::FAIL_PREPARE.with(|f| f.set(false));
}

/// The final values are the same with live values on and off: everything
/// the plan's digest does not reach (exact scores, gates, clusters, units,
/// mode, promotion) is identical.
#[test]
fn the_package_is_the_same_with_live_values_on_and_off() {
    let on = with_live_cohort(Some(live_block()), |db, sim, seq| {
        run_to_close(sim, db, seq, 22);
        sim.packages_v3.pop().unwrap()
    });
    let off = with_live_cohort(None, |db, sim, seq| {
        run_to_close(sim, db, seq, 22);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_cohorts WHERE ?1 = ?1", seq), 0, "no setup without the block");
        sim.packages_v3.pop().unwrap()
    });
    same_but_manifest_and_draws(&on, &off);
}

/// A shown value the package does not carry: the package wins, the defect
/// is recorded with the subject only, counted as one more symbol in the
/// private terms, and, over the bound, sets the global stop. Nothing signed
/// changes and nothing refuses.
#[test]
fn a_prefinal_mismatch_is_recorded_and_counted_never_refused() {
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        run_watching(db, sim, seq, 3_000);
        let pkg = sim.packages_v3.last().unwrap().clone();
        let good = pkg.package.entrants[1].q.unwrap().get();
        // The package is unchanged: its own values check clean.
        let signed = vec![(2i64, Some(good))];
        assert_eq!(tx(db, |s| live::cross_check(s, seq, &signed)).unwrap(), Vec::new());
        // (N) A drifted value is caught.
        let drift = vec![(2i64, Some(good + pkg.package.quant.width))];
        let defects = tx(db, |s| live::cross_check(s, seq, &drift)).unwrap();
        assert_eq!(defects.len(), 1);
        let w = pkg.package.quant.width;
        // Within the bound: recorded and counted, no stop.
        tx(db, |s| live::record_defects(s, seq, &defects, 10, w, 1_000_000, 8, 1)).unwrap();
        let terms: Value = serde_json::from_str(
            &db.conn.query_row("SELECT terms FROM live_cohorts WHERE seq = ?1", [seq], |r| r.get::<_, String>(0)).unwrap(),
        )
        .unwrap();
        assert_eq!((terms["extra_releases"].as_u64(), terms["over_bound"].as_bool()), (Some(1), Some(false)));
        let r = report(db);
        assert_eq!(r["defects"], json!([{"rule": live::PREFINAL_MISMATCH, "hotkey": hex::encode([0xd2; 32])}]));
        assert!(!r.to_string().contains(&(good + w).to_string()), "defect values stay private");
        assert_eq!(r["policy"]["stopped"], false);
        // Over the bound: the over_bound fault and the global stop.
        tx(db, |s| live::record_defects(s, seq, &defects, 10, w, 1, 1, 1)).unwrap();
        let r = report(db);
        assert_eq!((r["fault"].as_str(), r["policy"]["stopped"].as_bool()), (Some(live::FAULT_OVER_BOUND), Some(true)));
        assert!(r["prefinal"].is_null());
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop WHERE scope = '*' AND stopped = 1 AND ?1 = ?1", seq), 1);
        // The operator resumes on the same route. The cohort's own fault
        // still stops its values until the operator clears it there too.
        let req = |cohort, clear_fault| live::StopRequest { stopped: false, cohort, reason: "defect fixed".into(), clear_fault };
        tx(db, |s| live::set_stop(s, &req(None, false))).unwrap();
        let r = report(db);
        assert_eq!((r["fault"].as_str(), r["policy"]["stopped"].as_bool()), (Some(live::FAULT_OVER_BOUND), Some(false)));
        assert!(r["prefinal"].is_null());
        let out = tx(db, |s| live::set_stop(s, &req(Some(2), true))).unwrap();
        assert_eq!(out["cleared"], live::FAULT_OVER_BOUND);
        let r = report(db);
        assert_eq!(r["fault"], Value::Null);
        assert!(r["prefinal"]["current"].as_str().is_some(), "{r}");
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE cleared = 'over_bound' AND scope = CAST(?1 AS TEXT)", seq), 1);
    });
}

/// The private recount is recorded at every package, defect or not
/// (design 6.2 item 6).
#[test]
fn the_private_recount_is_recorded_at_every_package() {
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        run_watching(db, sim, seq, 3_000);
        let terms: Value = serde_json::from_str(
            &db.conn.query_row("SELECT terms FROM live_cohorts WHERE seq = ?1", [seq], |r| r.get::<_, String>(0)).unwrap(),
        )
        .unwrap();
        let live_bits = q1(db, "SELECT live_bits FROM live_cohorts WHERE seq = ?1", seq);
        assert_eq!(terms["extra_releases"].as_u64(), Some(0), "{terms}");
        assert_eq!(terms["over_bound"].as_bool(), Some(false));
        assert_eq!(terms["live"].as_i64(), Some(live_bits));
        assert!(terms["recount"].as_u64().unwrap() > 0);
        assert_eq!(terms["total"].as_u64(), Some(terms["recount"].as_u64().unwrap() + live_bits as u64));
    });
}

/// Ticks until the cohort has sealed (its jobs exist); the live step of
/// that tick has not run yet.
fn tick_to_seal(db: &Sqlite, sim: &mut Sim<'_>, seq: i64) {
    for _ in 0..3_000 {
        if q1(db, "SELECT COUNT(*) FROM jobs WHERE seq = ?1", seq) > 0 {
            return;
        }
        sim.tick();
    }
    panic!("cohort {seq} did not seal");
}

/// Ticks until the cohort reaches `transition`.
fn tick_to_transition(db: &Sqlite, sim: &mut Sim<'_>, seq: i64) {
    for _ in 0..3_000 {
        if phase(db, seq) == "transition" {
            return;
        }
        sim.tick();
    }
    panic!("cohort {seq} did not reach transition: {}", phase(db, seq));
}

/// (review M1, no self-lock) A release fault stops every live value of its
/// cohort, and no stop or resume lifts it; the operator's `clear_fault` on
/// the same journaled route does, after which each due value is released
/// once, in order. A clear without a cohort is refused, a clear with nothing
/// to clear is journaled, and a fault whose cause remains comes back at the
/// next failing step.
#[test]
fn the_operator_clears_a_release_fault_on_the_stop_route() {
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        tick_to_seal(db, sim, seq);
        tx(db, |s| live::prepare(s, seq)).unwrap();
        tx(db, |s| live::note_fault(s, seq, live::FAULT_RELEASE)).unwrap();
        let req = |stopped, cohort, clear_fault| live::StopRequest { stopped, cohort, reason: "fault drill".into(), clear_fault };
        assert!(tx(db, |s| live::set_stop(s, &req(false, None, true))).is_err(), "a clear names its cohort");
        for (stopped, cohort) in [(true, None), (false, None), (true, Some(2)), (false, Some(2))] {
            tx(db, |s| live::set_stop(s, &req(stopped, cohort, false))).unwrap();
        }
        tick_to_transition(db, sim, seq);
        assert_eq!(releases(db, seq), Vec::new(), "a stop or resume alone leaves the fault in place");
        let r = report(db);
        assert_eq!((r["fault"].as_str(), r["policy"]["stopped"].as_bool()), (Some(live::FAULT_RELEASE), Some(false)));
        // The clear, with the cohort stopped: the fault goes, the stop holds.
        let out = tx(db, |s| live::set_stop(s, &req(true, Some(2), true))).unwrap();
        assert_eq!((out["cleared"].as_str(), out["setup"].is_null()), (Some(live::FAULT_RELEASE), true));
        sim.tick();
        assert_eq!(releases(db, seq), Vec::new(), "the cohort's stop still holds");
        assert_eq!(report(db)["fault"], Value::Null);
        tx(db, |s| live::set_stop(s, &req(false, Some(2), false))).unwrap();
        sim.tick();
        let rel = releases(db, seq);
        for job in [0i64, 1, 2, 3] {
            let cps: Vec<i64> = rel.iter().filter(|x| x.0 == job && x.1 == "reading").map(|x| x.2).collect();
            assert_eq!(cps, [1, 2, 3], "job {job}: each checkpoint once, in order");
        }
        assert_eq!(rel.iter().filter(|x| x.1 == "prefinal").count(), 4);
        let r = report(db);
        assert!(r["fault"].is_null() && !r["prefinal"].is_null(), "{r}");
        // Journaled: four stops and resumes, the clear, the resume.
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE ?1 = ?1", seq), 6);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE cleared = 'release' AND retry = 0 AND ?1 = ?1", seq), 1);
        // Nothing left to clear: answered and journaled, nothing changes.
        let out = tx(db, |s| live::set_stop(s, &req(false, Some(2), true))).unwrap();
        assert!(out["cleared"].is_null());
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE ?1 = ?1", seq), 7);
        assert_eq!(releases(db, seq), rel);
        // A cause that remains records the fault again at the next failing
        // step, and the same clear lifts it again.
        tx(db, |s| live::note_fault(s, seq, live::FAULT_RELEASE)).unwrap();
        assert_eq!(report(db)["fault"], live::FAULT_RELEASE);
        let out = tx(db, |s| live::set_stop(s, &req(false, Some(2), true))).unwrap();
        assert_eq!(out["cleared"], live::FAULT_RELEASE);
    });
}

/// (review M1, no self-lock) A failed setup is the operator's to run
/// again: the clear marks it for a retry, and the next tick sets the cohort
/// up over the same sealed values and releases the readings in order; the
/// pre-finalized values released meanwhile stay exactly as they were. A
/// stopping fault recorded after a failed setup takes its place, and its
/// clear also runs the setup again. A retry that fails records its fault
/// and waits for the next clear.
#[test]
fn a_cleared_failed_setup_runs_again_at_the_next_tick() {
    live::FAIL_PREPARE.with(|f| f.set(true));
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        for _ in 0..3_000 {
            if q1(db, "SELECT COUNT(*) FROM live_cohorts WHERE seq = ?1", seq) > 0 {
                break;
            }
            sim.tick();
        }
        live::FAIL_PREPARE.with(|f| f.set(false));
        let setup = || -> (i64, Option<String>, String) {
            db.conn
                .query_row("SELECT segments, fault, setup FROM live_cohorts WHERE seq = ?1", [seq], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .unwrap()
        };
        assert_eq!(setup(), (0, Some(live::FAULT_PREPARE.into()), "failed".into()));
        tick_to_transition(db, sim, seq);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND kind = 'reading'", seq), 0);
        let prefinal: Vec<_> = releases(db, seq).into_iter().filter(|x| x.1 == "prefinal").collect();
        assert_eq!(prefinal.len(), 4);
        let shown = report(db)["prefinal"].clone();
        assert!(!shown.is_null());
        // A release fault takes the setup fault's place (it stops more);
        // the setup stays failed.
        tx(db, |s| live::note_fault(s, seq, live::FAULT_RELEASE)).unwrap();
        assert_eq!(setup(), (0, Some(live::FAULT_RELEASE.into()), "failed".into()));
        let req = live::StopRequest { stopped: false, cohort: Some(2), reason: "setup fixed".into(), clear_fault: true };
        let out = tx(db, |s| live::set_stop(s, &req)).unwrap();
        assert_eq!((out["cleared"].as_str(), out["setup"].as_str()), (Some(live::FAULT_RELEASE), Some("retry")));
        assert_eq!(setup(), (0, None, "retry".into()));
        assert_eq!(tx(db, live::pending_setup).unwrap(), vec![seq]);
        // The retry fails: its fault is recorded and nothing retries it.
        live::FAIL_PREPARE.with(|f| f.set(true));
        sim.tick();
        live::FAIL_PREPARE.with(|f| f.set(false));
        assert_eq!(setup(), (0, Some(live::FAULT_PREPARE.into()), "failed".into()));
        assert!(tx(db, live::pending_setup).unwrap().is_empty());
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND kind = 'reading'", seq), 0);
        // The next clear runs it again, and it succeeds.
        let out = tx(db, |s| live::set_stop(s, &req)).unwrap();
        assert_eq!((out["cleared"].as_str(), out["setup"].as_str()), (Some(live::FAULT_PREPARE), Some("retry")));
        sim.tick();
        assert_eq!(setup(), (4, None, "done".into()));
        assert_eq!(q1(db, "SELECT COUNT(*) FROM docs WHERE seq = ?1 AND live_segment IS NOT NULL AND docset = 'batch'", seq), 40);
        let rel = releases(db, seq);
        for job in [0i64, 1, 2, 3] {
            let cps: Vec<i64> = rel.iter().filter(|x| x.0 == job && x.1 == "reading").map(|x| x.2).collect();
            assert_eq!(cps, [1, 2, 3], "job {job}: each checkpoint once, in order");
        }
        let after: Vec<_> = rel.into_iter().filter(|x| x.1 == "prefinal").collect();
        assert_eq!(after, prefinal, "pre-finalized values are never recomputed");
        let r = report(db);
        assert_eq!(r["prefinal"], shown);
        assert_eq!(r["reference"]["checkpoints"].as_array().unwrap().len(), 3);
        assert!(r["fault"].is_null());
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE cleared = 'release' AND retry = 1 AND ?1 = ?1", seq), 1);
        assert_eq!(q1(db, "SELECT COUNT(*) FROM live_stop_log WHERE cleared = 'prepare' AND retry = 1 AND ?1 = ?1", seq), 1);
        assert!(tx(db, live::pending_setup).unwrap().is_empty());
    });
    live::FAIL_PREPARE.with(|f| f.set(false));
}

/// A plan's live block validates with the plan: the rule, the segment
/// count, a positive epsilon and a scored cohort with a quant block.
#[test]
fn live_blocks_validate_with_their_plan() {
    let weak = crate::eval::plan::tests::artifact(crate::eval::plan::tests::identity(0, 0xb0), 0x22);
    let probe = crate::eval::plan::ProbeRef { version: 1, size: 1024, key: "probes/v1/record.json".into(), sha256: hex::encode([1; 32]) };
    let ceil = Ceilings { accept_reward_v0: false, ..cfg().ceilings };
    let with = |l: LivePlan| {
        let mut p = plan_v3(2, "testnet-rv1-live-ok", 1000, weak.clone());
        p.scorer.chunk_docs = 32;
        p.fanout.as_mut().unwrap().score.lane_docs = 1000;
        let v = p.reward_v1.as_mut().unwrap();
        v.probes = vec![probe.clone()];
        v.canary.draw_humans = 200;
        v.live = Some(l);
        p.validate(ceil)
    };
    with(live_block()).unwrap();
    with(LivePlan { segments_max: 0, eps_live_milli: 0, ..live_block() }).unwrap();
    for bad in [
        LivePlan { rule: "live-v2".into(), ..live_block() },
        LivePlan { segments_max: 1, ..live_block() },
        LivePlan { eps_live_milli: 0, ..live_block() },
        LivePlan { segments_max: 32, ..live_block() },
    ] {
        assert!(with(bad.clone()).is_err(), "{bad:?}");
    }
    // A stored plan without the block deserializes and runs as before; an
    // unknown field in the block is refused.
    let mut v: Value = serde_json::to_value(plan_v3(2, "x", 1000, weak.clone())).unwrap();
    assert!(v["reward_v1"].get("live").is_none());
    v["reward_v1"]["live"] = json!({"rule": "live-v1", "prefinal": true, "eps_live_milli": 1000, "segments_max": 4, "rank": true});
    assert!(serde_json::from_value::<CohortPlan>(v).is_err());
}

/// Arena witness (`--features arena-witness`): one release and the live
/// report run within their footprints (`arena_routes::live_step_bytes` at
/// the cohort's batch, and the cohort control footprint for the report),
/// with no allocation outside the scope, at this test's shape. The widest
/// roster's witnesses follow
/// (`arena_witness_live_steps_fit_at_the_widest_roster`).
#[cfg(feature = "arena-witness")]
#[test]
fn arena_witness_live_steps_fit_their_footprint() {
    use crate::arena_routes::{COHORT_CONTROL_BYTES, R_COHORT_FETCH, R_COHORT_LIVE, live_step_bytes};
    slop_ninja_arena::set_mode(slop_ninja_arena::Mode::Enforce);
    with_live_cohort(Some(live_block()), |db, sim, seq| {
        run_watching(db, sim, seq, 3_000);
        db.conn.execute("DELETE FROM live_releases WHERE seq = ?1", [seq]).unwrap();
        db.conn.execute("UPDATE live_cohorts SET checked_preds = -1, quant_width = NULL, margin_line = NULL WHERE seq = ?1", [seq]).unwrap();
        let c = cfg();
        let docs = tx(db, |s| live::cohort_docs(s, seq)).unwrap();
        let released = crate::arena_witness::witness(R_COHORT_LIVE, live_step_bytes(docs), || {
            tx(db, |s| live::release_due(s, &c, seq)).unwrap()
        });
        assert!(released);
        while tx(db, |s| live::release_due(s, &c, seq)).unwrap() {}
        let rows = crate::arena_witness::witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
            tx(db, live::report).unwrap()["rows"].as_array().map_or(0, Vec::len)
        });
        assert_eq!(rows, 3);
    });
}

/// The widest live shape: `MAX_ROSTER` executed entrants (distinct
/// hotkeys and artifacts) and `R_k` over a batch of `n` documents, on a
/// deployment without a roster ceiling, with readings (`L >= 2`) and
/// pre-finalized values. One entrant in sixteen crashes on every request,
/// so the report carries withheld symbols beside values. Calls `f` with the
/// cohort open.
#[cfg(feature = "arena-witness")]
fn with_widest_live_cohort<T>(n: u32, f: impl FnOnce(&Sqlite, &mut Sim<'_>, i64, &EvalConfig) -> T) -> T {
    let db = Sqlite::open();
    let (mut sim, weak, probe_ref) = probe_built_doc(&db, Some(crate::eval::plan::TASK_ORDER_SEEDED_RANK));
    let mut c = cfg();
    c.ceilings.roster_cap = None;
    sim.cfg = c.clone();
    let roster = u16::try_from(usize::from(slop_ninja_protocol::MAX_ROSTER)).unwrap();
    let ids: Vec<crate::eval::plan::IdentityJson> = (0..roster)
        .map(|k| {
            let mut hotkey = [0xe5u8; 32];
            hotkey[..2].copy_from_slice(&k.to_be_bytes());
            crate::eval::plan::IdentityJson {
                uid: 100 + k,
                hotkey: format!("0x{}", hex::encode(hotkey)),
                registration_block: 2000 + u64::from(k),
            }
        })
        .collect();
    // Every entrant's calibration differs (its content digest), and none
    // equals R_k's files.
    let entrants: Vec<_> = ids.iter().enumerate().map(|(k, id)| v3_artifact(&mut sim, id.clone(), [1, 7, 3, k as u8, 5])).collect();
    let mut p = plan_v3(2, "testnet-rv1-live-wide", n, weak);
    p.entrants = entrants;
    p.allow_list = ids;
    {
        let v = p.reward_v1.as_mut().unwrap();
        v.probes = vec![probe_ref];
        v.canary.draw_humans = 8;
        v.task_order = Some(crate::eval::plan::TASK_ORDER_SEEDED_RANK.into());
        v.live = Some(live_block());
    }
    sim.predict_doc = Some(Box::new(|job, kind, sha| {
        let c = doc_conf((job % 7) as u8, kind, sha, (30.0, 65.0, 75.0), 12.0);
        (c, c * 0.4)
    }));
    sim.score = Box::new(|q| if q.job % 16 == 5 { Score::Crash } else { Score::Ok });
    let seq = open_with(&db, &c, &p, 20, sim.now);
    f(&db, &mut sim, seq, &c)
}

/// Arena witness (`--features arena-witness`) at the widest roster: the
/// live setup over the cohort's full batch and every release of
/// `MAX_ROSTER` entrants and `R_k`, each within
/// `arena_routes::live_step_bytes` at that batch (each is its own child
/// scope in the step), and the `/eval/live` report at `MAX_ROSTER` rows
/// within the cohort control footprint, with no allocation outside the
/// scope. The cohort runs to transition first, so the setup and the
/// releases are run again over its finished outcomes: the heaviest
/// releases (a full batch read for each pre-finalized value, `R_k`'s with
/// the manifest's parameters rebuilt) all come due at once.
///
/// Measured 2026-11-10 at 40 documents and 256 entrants, with the stored
/// seal state's published index inflated to 2 MiB: the setup peaks at
/// 278,727 B live (5 pages), each reading at 301,010 B and each entrant's
/// pre-finalized value at 301,045 B (6 pages), `R_k`'s pre-finalized value
/// with the stored manifest at 362,852 B (7 pages), all within the
/// 1,089,536 B footprint (18 pages); the report at 256 rows at 709,355 B
/// (14 of 34 pages). The cohort's parsed plan (at most
/// `PLAN_PARSED_BYTES`, in the alarm's base) is most of each step's heap.
#[cfg(feature = "arena-witness")]
#[test]
fn arena_witness_live_steps_fit_at_the_widest_roster() {
    use crate::arena_routes::{COHORT_CONTROL_BYTES, R_COHORT_FETCH, R_COHORT_LIVE, live_step_bytes};
    slop_ninja_arena::set_mode(slop_ninja_arena::Mode::Enforce);
    let roster = usize::from(slop_ninja_protocol::MAX_ROSTER);
    with_widest_live_cohort(40, |db, sim, seq, c| {
        for _ in 0..20_000 {
            if phase(db, seq) == "transition" {
                break;
            }
            sim.tick();
        }
        assert_eq!(phase(db, seq), "transition", "fault {:?}", fault_of(db, seq));
        assert_eq!(q1(db, "SELECT COUNT(*) FROM jobs WHERE seq = ?1 AND role = 'entrant'", seq), roster as i64);
        let docs = tx(db, |s| live::cohort_docs(s, seq)).unwrap();
        // The live step reads only the subjects' digests, the batch size and
        // the manifest record of the stored seal state: its fields that grow
        // with the chain's history (here the published index at seal,
        // inflated past every live footprint) are never parsed.
        let mut st: Value = serde_json::from_str(&db.conn.query_row("SELECT v3 FROM cohorts WHERE seq = ?1", [seq], |r| r.get::<_, String>(0)).unwrap()).unwrap();
        st["published_at_seal"] = json!(["00".repeat(1 << 20), ""]);
        db.conn.execute("UPDATE cohorts SET v3 = ?1 WHERE seq = ?2", rusqlite::params![st.to_string(), seq]).unwrap();
        assert!(q1(db, "SELECT length(v3) FROM cohorts WHERE seq = ?1", seq) > 2 * live_step_bytes(docs) as i64);
        // The setup again, over the same sealed values.
        db.conn.execute("DELETE FROM live_releases WHERE seq = ?1", [seq]).unwrap();
        db.conn.execute("DELETE FROM live_cohorts WHERE seq = ?1", [seq]).unwrap();
        db.conn.execute("UPDATE docs SET live_segment = NULL WHERE seq = ?1", [seq]).unwrap();
        crate::arena_witness::witness(R_COHORT_LIVE, live_step_bytes(docs), || tx(db, |s| live::prepare(s, seq)).unwrap());
        let segments = q1(db, "SELECT segments FROM live_cohorts WHERE seq = ?1", seq);
        assert!(segments >= i64::from(slop_ninja_protocol::live::MIN_SEGMENTS), "readings at the widest roster: L = {segments}");
        assert_eq!(q1(db, "SELECT COUNT(*) FROM docs WHERE seq = ?1 AND live_segment IS NOT NULL", seq), docs as i64);
        // Every release, each in its own scope, as the step runs them.
        let mut released = 0usize;
        while crate::arena_witness::witness(R_COHORT_LIVE, live_step_bytes(docs), || tx(db, |s| live::release_due(s, c, seq)).unwrap()) {
            released += 1;
        }
        // L - 1 readings for each entrant and R_k, and one pre-finalized
        // value for each of them.
        let readings = (roster + 1) * usize::try_from(segments - 1).unwrap();
        assert_eq!(released, readings + roster + 1);
        let withheld = q1(db, "SELECT COUNT(*) FROM live_releases WHERE seq = ?1 AND kind = 'prefinal' AND outcome = 'withheld'", seq);
        assert!(withheld > 0 && withheld < roster as i64, "{withheld} withheld");
        // The report at MAX_ROSTER rows.
        let rows = crate::arena_witness::witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
            let r = tx(db, live::report).unwrap();
            assert!(r.get("refused").is_none(), "{r}");
            r["rows"].as_array().map_or(0, Vec::len)
        });
        assert_eq!(rows, roster);
        assert_eq!(rows, live::ROWS);
    });
}

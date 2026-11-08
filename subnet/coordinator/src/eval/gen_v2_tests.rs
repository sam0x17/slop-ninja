//! Generation v2 cohorts end to end in the native Sim, with fake vendors (no
//! vendor, Workers AI or Cloudflare call): the draw's rotation and voice
//! split, the lanes' requests, the commit's outcome classes, the vendor
//! pause, spend by vendor and its ledger keys, and the private records at
//! the seal. The plan field stays refused by this build's validation
//! (`plan::GENERATION_V2_REFUSAL`): these tests store a v2 plan directly,
//! the one path the flag will open.

use super::*;
use crate::eval::gen_v2::{self, DocGen};
use slop_ninja_gen_core::manifest::{Manifest, ObjectRef, Register, fixture as mfx};
use slop_ninja_gen_core::outcome::{GenCall, GenReply, OutcomeCode};
use slop_ninja_gen_core::plan::{FrontierLanes, FrontierMode, GenerationPlan};
use slop_ninja_gen_core::record::GenRecord;
use slop_ninja_gen_core::spend::Usage;
use slop_ninja_gen_core::vendor::{Effort, Lane, Vendor};
use slop_ninja_gen_core::voice::{VoiceSplit, fixture as vfx};

/// The user message of a v2 request body (each vendor's shape).
fn user_message(call: &GenCall) -> String {
    let b: Value = serde_json::from_str(&call.request.body).unwrap();
    let m = match call.request.vendor {
        Vendor::Openai => &b["input"][0]["content"],
        Vendor::Anthropic => &b["messages"][0]["content"],
        Vendor::WorkersAi => &b["messages"][1]["content"],
    };
    m.as_str().unwrap().to_owned()
}

/// A vendor that answers every call: a rewrite reverses the passage's words
/// (the passage is the prompt's last line in every fixture template), a
/// scratch document is the target's length of prose. Usage is reported.
pub(super) fn fake_vendor(it: &engine::GenItem, call: &GenCall) -> GenReply {
    let user = user_message(call);
    let text = if it.kind == "mixed" {
        let passage = user.rsplit('\n').next().unwrap();
        let mut w: Vec<&str> = passage.split_whitespace().collect();
        w.reverse();
        w.join(" ")
    } else {
        let n: usize = user.rsplit("about ").next().unwrap().split(' ').next().unwrap().parse().unwrap();
        vec!["prose"; n].join(" ")
    };
    answer_reply(call, text)
}

fn answer_reply(call: &GenCall, text: String) -> GenReply {
    let mut g = GenReply::without_answer(OutcomeCode::Answer, &call.request, 900);
    g.http_status = 200;
    g.sent = true;
    g.usage_reported = true;
    g.usage = Usage { input_tokens: 300, cached_input_tokens: 0, output_tokens: 200, reasoning_tokens: None };
    g.visible_bytes = text.len() as u32;
    g.model_reported = Some(call.request.model.clone());
    g.text = Some(text);
    g
}

fn failed_reply(call: &GenCall, outcome: OutcomeCode, status: u16) -> GenReply {
    let mut g = GenReply::without_answer(outcome, &call.request, 50);
    g.http_status = status;
    g.sent = true;
    g
}

/// The measured fixture manifest with a register for every pool domain, and
/// the fixture voice library, put in the sim's eval bucket; with `total`,
/// the library is padded with entries of the fixture's longest text
/// (`max_entry_bytes`) until the manifest and the library together reach
/// `total` declared bytes (within one entry).
/// How [`put_objects_padded`] pads the voice library to its total: entries
/// of the longest text the header allows, or the shortest entries the
/// library schema allows (one-byte text and source, the shortest distinct
/// ids, the shortest category and kinds), the most entries (and the most
/// per-entry overhead) per declared byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(feature = "arena-witness"), allow(dead_code))]
enum Pad {
    Long,
    Short,
}

/// The shortest distinct id of entry `n` (radix 36; no fixture id is one).
fn short_id(mut n: usize) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[n % 36]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn put_objects_padded(sim: &mut Sim<'_>, total: Option<u64>, pad: Pad) -> (ObjectRef, Manifest) {
    let mut lib = vfx::library();
    if let Some(total) = total {
        let base = lib.entries.clone();
        let short_base = base
            .iter()
            .map(|e| {
                let mut e = e.clone();
                e.id = String::new();
                e.text = "x".into();
                e.source = "s".into();
                e.imitated_author = None;
                e.kinds.truncate(1);
                e
            })
            .min_by_key(|e| serde_json::to_vec(e).unwrap().len())
            .unwrap();
        let manifest_bytes = {
            let m = mfx::with_voice_library(mfx::measured(mfx::planned()), vfx::reference(&lib));
            serde_json::to_vec(&m).unwrap().len() as u64 + 200
        };
        let mut len = manifest_bytes + vfx::to_jsonl(&lib).len() as u64;
        let mut n = 0usize;
        loop {
            let e = match pad {
                Pad::Long => {
                    let mut e = base[n % base.len()].clone();
                    e.id = format!("pad_{n}");
                    let head = format!("TEST voice pad {n} ");
                    e.text = head.clone() + &"x".repeat(lib.header.max_entry_bytes as usize - head.len());
                    e
                }
                Pad::Short => {
                    let mut e = short_base.clone();
                    e.id = short_id(n);
                    e
                }
            };
            let line = serde_json::to_vec(&e).unwrap().len() as u64 + 1;
            if len + line > total {
                break;
            }
            len += line;
            lib.entries.push(e);
            n += 1;
        }
    }
    let lb = vfx::to_jsonl(&lib);
    let lref = vfx::reference(&lib);
    sim.eval.insert(lref.key.clone(), lb);
    let mut m = mfx::with_voice_library(mfx::measured(mfx::planned()), lref);
    m.registers.push(Register { domain: "wikinews".into(), register: "a TEST news brief".into(), topics: vec!["a vote".into(), "a storm".into()] });
    let mb = serde_json::to_vec(&m).unwrap();
    let r = ObjectRef { key: "generation/manifests/generator-manifest-v1.json".into(), sha256: lane_logic::sha_hex(&mb), bytes: mb.len() as u64 };
    sim.eval.insert(r.key.clone(), mb);
    (r, m)
}

fn gen_plan(manifest: ObjectRef) -> GenerationPlan {
    let mut v = serde_json::to_value(GenerationPlan {
        schema: slop_ninja_gen_core::GENERATION_PLAN_SCHEMA.into(),
        manifest,
        rotation: slop_ninja_gen_core::ROTATION_VERSION.into(),
        frontier_mode: FrontierMode::Sync,
        frontier_lanes: FrontierLanes { lease_ms: 300_000, inflight_per_lane: None },
        voice_split: String::new(),
    })
    .unwrap();
    // A plan naming no split takes the default (25% no profile).
    v.as_object_mut().unwrap().remove("voice_split");
    serde_json::from_value(v).unwrap()
}

/// Queues `p` as a v1 plan, then stores it with `generation` (and the
/// manifest's Workers AI generators): what lifting the flag will admit.
fn open_v2(db: &Sqlite, p: &CohortPlan, g: GenerationPlan, m: &Manifest, first_epoch: u64, now: u64) -> i64 {
    let bytes = serde_json::to_vec(p).unwrap();
    let seq = tx(db, |s| engine::queue(s, &cfg(), &bytes, now)).unwrap();
    let mut v2 = p.clone();
    v2.generators = mfx::lane_ids(m, Lane::WorkersAi);
    v2.generation = Some(g);
    assert_eq!(v2.validate(cfg().ceilings).unwrap_err().0, super::super::plan::GENERATION_V2_REFUSAL, "the flag stays off");
    v2.validate_checks(cfg().ceilings).unwrap();
    let stored = serde_json::to_string(&v2).unwrap();
    db.conn
        .execute("UPDATE cohorts SET plan = ?1, plan_sha256 = ?2 WHERE seq = ?3", rusqlite::params![stored, lane_logic::sha_hex(stored.as_bytes()), seq])
        .unwrap();
    kick_at(db, first_epoch, now);
    kick_at(db, first_epoch + 1, now);
    kick_at(db, first_epoch + 2, now);
    seq
}

fn records(db: &Sqlite, seq: i64) -> Vec<(String, String, DocGen)> {
    db.conn
        .prepare("SELECT key, status, gen_record FROM docs WHERE seq = ?1 AND gen_record IS NOT NULL ORDER BY idx")
        .unwrap()
        .query_map([seq], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?)))
        .unwrap()
        .map(|x| {
            let (k, st, g) = x.unwrap();
            (k, st, serde_json::from_str(&g).unwrap())
        })
        .collect()
}

fn v2_cohort<'a>(db: &'a Sqlite, label: &str) -> (Sim<'a>, i64, Manifest) {
    v2_cohort_sized(db, label, None)
}

fn v2_cohort_sized<'a>(db: &'a Sqlite, label: &str, total: Option<u64>) -> (Sim<'a>, i64, Manifest) {
    v2_cohort_padded(db, label, total, Pad::Long)
}

fn v2_cohort_padded<'a>(db: &'a Sqlite, label: &str, total: Option<u64>, pad: Pad) -> (Sim<'a>, i64, Manifest) {
    let (mut sim, weak, probe_ref) = probe_built(db);
    let (r, m) = put_objects_padded(&mut sim, total, pad);
    let mut p = plan_v3(2, label, 40, weak);
    p.reward_v1.as_mut().unwrap().probes = vec![probe_ref];
    p.reward_v1.as_mut().unwrap().canary.draw_humans = 16;
    let seq = open_v2(db, &p, gen_plan(r), &m, 20, sim.now);
    (sim, seq, m)
}

#[test]
fn a_v2_cohort_runs_end_to_end_with_rotated_cells_private_records_and_vendor_spend() {
    let db = Sqlite::open();
    let (mut sim, seq, m) = v2_cohort(&db, "testnet-g2-a");
    run_to_close(&mut sim, &db, seq, 22);
    assert_eq!(fault_of(&db, seq), None);
    let recs = records(&db, seq);
    let generated: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM docs WHERE seq = ?1 AND kind IN ('mixed','model_only') AND docset IN ('batch','spare')", [seq], |r| r.get(0))
        .unwrap();
    assert_eq!(recs.len() as i64, generated, "every generated document has its record");
    // Each stream is half frontier, half Workers AI (within one), and its
    // durable state advanced exactly once per document.
    let mut by_stream: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for (_, _, g) in &recs {
        let e = by_stream.entry(g.record.assignment.stream.clone()).or_default();
        match g.record.assignment.lane {
            Lane::Frontier => e.0 += 1,
            Lane::WorkersAi => e.1 += 1,
        }
        assert_eq!(g.record.assignment.voice_library_version, 1);
        assert!(!gen_v2::encode(g).unwrap().contains("TEST voice"), "no prompt or voice text is recorded");
    }
    for (stream, (f, w)) in &by_stream {
        assert!(f.abs_diff(*w) <= 1, "{stream}: {f} frontier, {w} Workers AI");
        let st: slop_ninja_gen_core::rotation::StreamState = serde_json::from_str(
            &db.conn.query_row("SELECT state FROM gen_rotation WHERE stream = ?1", [stream], |r| r.get::<_, String>(0)).unwrap(),
        )
        .unwrap();
        assert_eq!(st.draws as i64, f + w, "{stream}: one advance per document");
    }
    assert!(by_stream.values().any(|(f, _)| *f > 0) && by_stream.values().any(|(_, w)| *w > 0));
    // Every accepted document's final record matches the stored text.
    let ready: Vec<_> = recs.iter().filter(|(_, st, _)| st == "ready").collect();
    assert!(!ready.is_empty());
    for (key, _, g) in &ready {
        let (sha, model_id): (String, String) = db
            .conn
            .query_row("SELECT text_sha256, generator FROM docs WHERE seq = ?1 AND key = ?2", rusqlite::params![seq, key], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let f = g.record.final_.as_ref().unwrap();
        assert_eq!(f.text_sha256, sha);
        assert_eq!(model_id, g.record.attempts[f.attempt as usize].model_requested);
        assert!(m.model(&model_id).is_some());
    }
    // The private index (v4) binds the generation shards by digest; each
    // line is a generation record.
    let label = "testnet-g2-a";
    let index: Value = serde_json::from_slice(&sim.eval[&format!("{}/index.json", engine::batch_prefix(label))]).unwrap();
    assert_eq!(index["schema"], gen_v2::BATCH_INDEX_SCHEMA);
    let block = &index["generation"];
    assert_eq!(block["schema"], slop_ninja_gen_core::RECORD_SCHEMA);
    assert_eq!(block["voice_split"], VoiceSplit::DEFAULT.code());
    let mut lines = 0;
    for sh in block["shards"].as_array().unwrap() {
        let body = &sim.eval[sh["key"].as_str().unwrap()];
        assert!(sh["key"].as_str().unwrap().starts_with(&format!("cohorts/{label}/generation/")));
        assert_eq!(lane_logic::sha_hex(body), sh["sha256"].as_str().unwrap());
        for l in std::str::from_utf8(body).unwrap().lines() {
            let r: GenRecord = serde_json::from_str(l).unwrap();
            assert_eq!(r.schema, slop_ninja_gen_core::RECORD_SCHEMA);
            lines += 1;
        }
    }
    assert_eq!(lines, recs.len());
    // Nothing of generation reaches the receipts bucket (manifest, package,
    // published indexes, receipts, closure).
    for (k, v) in &sim.rcpt {
        let t = String::from_utf8_lossy(v);
        for needle in ["gen_record", "claude-", "gpt-6", "voice_split", slop_ninja_gen_core::RECORD_SCHEMA, "generation/"] {
            assert!(!t.contains(needle), "{k} names {needle}");
        }
    }
    // Spend by vendor: status, and the ledger keys at settle.
    let st = status(&db, sim.now);
    let c = st["cohorts"].as_array().unwrap().iter().find(|c| c["label"] == label).unwrap().clone();
    let by = &c["gen_cost_by_vendor"];
    let (oa, an, wa) = (by["openai"].as_u64().unwrap(), by["anthropic"].as_u64().unwrap(), by["workers_ai"].as_u64().unwrap());
    assert!(oa > 0 && an > 0 && wa > 0, "{by}");
    assert_eq!(oa + an + wa, c["generation_cost_microusd"].as_u64().unwrap());
    assert!(c["generation_v2"]["cells"].as_array().unwrap().len() > 2);
    let settled: BTreeMap<String, u64> = sim.settled.iter().cloned().collect();
    assert_eq!(settled.get(&format!("gen:openai:main:{label}")), Some(&oa));
    assert_eq!(settled.get(&format!("gen:anthropic:main:{label}")), Some(&an));
    assert!(!settled.contains_key(&format!("gen:workers_ai:main:{label}")), "Workers AI stays in the cohort's own key");
    let main = settled[&format!("main:{label}")];
    let main_cohort: u64 = db.conn.query_row("SELECT settled_microusd FROM cohorts WHERE seq = ?1", [seq], |r| r.get::<_, i64>(0)).unwrap() as u64;
    assert_eq!(main, main_cohort);
    assert!(main >= wa, "the cohort's key keeps the Workers AI generation");
    // The draw summary names the split and the assignments.
    let draw: Value = serde_json::from_str(&db.conn.query_row("SELECT draw FROM cohorts WHERE seq = ?1", [seq], |r| r.get::<_, String>(0)).unwrap()).unwrap();
    assert_eq!(draw["generation"]["voice_split"], "profile_none_quarter_v1");
}

#[test]
fn a_spend_cap_moves_pending_items_to_workers_ai_refused_cells_disable_and_refusals_rotate() {
    let db = Sqlite::open();
    let (mut sim, seq, m) = v2_cohort(&db, "testnet-g2-b");
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(String, String)>::new()));
    let seen = calls.clone();
    sim.vendor = Box::new(move |it, call| {
        let model = call.request.model.clone();
        seen.borrow_mut().push((model.clone(), call.request.effort.code().to_owned()));
        match call.request.vendor {
            // Anthropic's organization reached its spend cap.
            Vendor::Anthropic => failed_reply(call, OutcomeCode::SpendCap, 429),
            // One OpenAI model refuses its requests (400): its cell is disabled.
            Vendor::Openai if model == "gpt-6-astra" => failed_reply(call, OutcomeCode::CellRefused, 400),
            // Another refuses the content once per document.
            Vendor::Openai if model == "gpt-6-luna" => {
                let mut g = failed_reply(call, OutcomeCode::Refusal, 200);
                g.usage_reported = true;
                g.usage = Usage { input_tokens: 100, cached_input_tokens: 0, output_tokens: 3, reasoning_tokens: None };
                g
            }
            _ => fake_vendor(it, call),
        }
    });
    run_to_close(&mut sim, &db, seq, 22);
    assert_eq!(fault_of(&db, seq), None, "the cohort never waits on a vendor");
    let v = tx(&db, gen_v2::vendor_states).unwrap();
    assert!(matches!(v.anthropic, slop_ninja_gen_core::pause::VendorState::Paused { cause: slop_ninja_gen_core::rotation::FallbackCause::SpendCap, .. }));
    assert_eq!(v.openai, slop_ninja_gen_core::pause::VendorState::Active);
    let recs = records(&db, seq);
    let anthropic = |id: &str| m.model(id).unwrap().vendor == Vendor::Anthropic;
    let mut moved = 0;
    for (key, st, g) in &recs {
        let a = &g.record.assignment;
        // Every document drawn on Anthropic ended on Workers AI, recorded.
        if anthropic(a.drawn().0) {
            let f = a.fallback.as_ref().unwrap_or_else(|| panic!("{key} kept its Anthropic cell"));
            assert_eq!(f.cause, slop_ninja_gen_core::rotation::FallbackCause::SpendCap);
            assert_eq!(a.call_lane(), Lane::WorkersAi);
            assert_eq!(a.lane, Lane::Frontier, "the drawn lane still counts as frontier");
            moved += 1;
        }
        assert!(!(st == "ready" && anthropic(&g.call.model)), "{key}");
        // A refused cell is never called twice in the cohort.
        let astra = g.record.attempts.iter().filter(|x| x.model_requested == "gpt-6-astra").count();
        assert!(astra <= 1, "{key}");
    }
    assert!(moved > 0);
    // No document called a capped vendor twice, and no retry chose a
    // paused vendor's model: nothing loops on a cap.
    let anth_calls = calls.borrow().iter().filter(|(m, _)| anthropic(m)).count();
    assert!(anth_calls >= 1);
    for (key, _, g) in &recs {
        let capped: Vec<_> = g.record.attempts.iter().filter(|x| anthropic(&x.model_requested)).collect();
        assert!(capped.len() <= 1, "{key}");
        if let Some(c) = capped.first() {
            assert_eq!(c.outcome, OutcomeCode::SpendCap);
            assert!(g.record.attempts.iter().skip_while(|x| x.attempt <= c.attempt).all(|x| !anthropic(&x.model_requested)), "{key}");
        }
    }
    let disabled: Vec<(String, String)> = db
        .conn
        .prepare("SELECT model, effort FROM gen_disabled WHERE seq = ?1")
        .unwrap()
        .query_map([seq], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(disabled.iter().all(|(m, _)| m == "gpt-6-astra"));
    let astra_calls: BTreeSet<(String, String)> = calls.borrow().iter().filter(|(m, _)| m == "gpt-6-astra").cloned().collect();
    assert_eq!(astra_calls.len(), disabled.len(), "every refused cell was disabled");
    // No pending document is left on a disabled cell.
    for (key, st, g) in &recs {
        if st == "pending" {
            assert!(!disabled.contains(&(g.call.model.clone(), g.call.effort.code().to_owned())), "{key}");
        }
    }
    // A content refusal used an attempt and the next model of the lane answered.
    let refused: Vec<_> = recs.iter().filter(|(_, _, g)| g.record.attempts.iter().any(|x| x.outcome == OutcomeCode::Refusal)).collect();
    for (key, st, g) in &refused {
        assert_eq!(st, "ready", "{key}");
        let last = g.record.attempts.last().unwrap();
        assert_ne!(last.model_requested, "gpt-6-luna");
    }
    let rejects: String = db.conn.query_row("SELECT group_concat(rejects, '') FROM docs WHERE seq = ?1", [seq], |r| r.get::<_, Option<String>>(0)).unwrap().unwrap_or_default();
    assert_eq!(rejects.matches("refusal;").count(), refused.len());
    // The capped and refused calls charged nothing (not generated); only
    // answered calls and the refusals' reported usage are charged.
    let anth: i64 = db.conn.query_row("SELECT COALESCE(SUM(cost_microusd), 0) FROM gen_spend WHERE seq = ?1 AND vendor = 'anthropic'", [seq], |r| r.get(0)).unwrap();
    assert_eq!(anth, 0);
    // An operator clear (`/eval/gen/vendor-clear`) resumes the vendor for later draws.
    assert_eq!(gen_v2::pausable_vendor("anthropic"), Some(Vendor::Anthropic));
    assert_eq!(gen_v2::pausable_vendor("workers_ai"), None, "Workers AI never pauses deployment-wide");
    assert!(tx(&db, |s| gen_v2::resume_vendor(s, Vendor::Anthropic)).unwrap());
    assert!(!tx(&db, |s| gen_v2::resume_vendor(s, Vendor::Anthropic)).unwrap(), "idempotent");
    assert_eq!(tx(&db, gen_v2::vendor_states).unwrap().anthropic, slop_ninja_gen_core::pause::VendorState::Active);
}

#[test]
fn frontier_outages_move_the_item_to_workers_ai_at_the_bound_and_never_pause_the_cohort() {
    let db = Sqlite::open();
    let (mut sim, seq, m) = v2_cohort(&db, "testnet-g2-c");
    sim.vendor = Box::new(|it, call| match call.request.vendor {
        Vendor::Openai => failed_reply(call, OutcomeCode::Outage, 503),
        _ => fake_vendor(it, call),
    });
    run_to_close(&mut sim, &db, seq, 22);
    assert_eq!(fault_of(&db, seq), None);
    for (key, _, g) in records(&db, seq) {
        let a = &g.record.assignment;
        if m.model(a.drawn().0).unwrap().vendor == Vendor::Openai {
            assert_eq!(a.fallback.as_ref().map(|f| f.cause), Some(slop_ninja_gen_core::rotation::FallbackCause::Outage), "{key}");
        }
    }
    // OpenAI stays active: an outage is not a cap.
    assert_eq!(tx(&db, gen_v2::vendor_states).unwrap().openai, slop_ninja_gen_core::pause::VendorState::Active);
}

#[test]
fn a_redelivered_v2_commit_is_fenced_without_a_second_charge_and_a_stale_call_is_charged_on_its_own_cell() {
    let db = Sqlite::open();
    let (mut sim, seq, _) = v2_cohort(&db, "testnet-g2-d");
    sim.frozen = vec!["gen"];
    let drawn = || -> i64 { db.conn.query_row("SELECT COUNT(*) FROM docs WHERE seq = ?1 AND gen_record IS NOT NULL", [seq], |r| r.get(0)).unwrap() };
    for _ in 0..20 {
        if drawn() > 0 {
            break;
        }
        sim.tick();
    }
    assert_eq!(phase(&db, seq), "generating");
    assert!(drawn() > 0);
    let c2 = cfg();
    let (lane, generation, name): (i64, i64, String) = db
        .conn
        .query_row("SELECT lane, gen, name FROM lanes WHERE seq = ?1 AND kind = 'gen' ORDER BY lane LIMIT 1", [seq], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    let r = LaneRef { seq, kind: "gen".into(), lane, generation };
    let wants = tx(&db, |s| lanes::gen_claim(s, &c2, &r, &name, &BTreeMap::new(), sim.now)).unwrap();
    let GenClaimReply::Wait { wants, .. } = wants else { panic!("{wants:?}") };
    let grants: BTreeMap<String, u32> = wants.keys().map(|k| (k.clone(), 1)).collect();
    let GenClaimReply::Items { items, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, &name, &grants, sim.now)).unwrap() else { panic!() };
    let it = items.iter().find(|it| it.call.is_some()).unwrap();
    let call = it.call.as_deref().unwrap();
    assert_eq!(call.request.request_sha256, lane_logic::sha_hex(call.request.body.as_bytes()));
    let res = vec![GenOutcome::from_reply(it, fake_vendor(it, call))];
    tx(&db, |s| lanes::gen_commit(s, &c2, &r, &res, sim.now)).unwrap();
    let doc = |idx: i64| -> (String, i64) {
        db.conn.query_row("SELECT status, attempts FROM docs WHERE seq = ?1 AND idx = ?2", rusqlite::params![seq, idx], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    };
    assert_eq!(doc(it.idx), ("ready".into(), 1));
    let spend = || -> i64 { db.conn.query_row("SELECT SUM(calls) FROM gen_spend WHERE seq = ?1", [seq], |r| r.get(0)).unwrap() };
    let before = spend();
    let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &res, sim.now)).unwrap();
    assert_eq!(v["fenced"], 1);
    assert_eq!(doc(it.idx), ("ready".into(), 1));
    assert_eq!(spend(), before, "an exact redelivery (the same claim token) records and charges nothing again");
    let g = tx(&db, |s| gen_v2::doc_gen(s, seq, it.idx)).unwrap().unwrap();
    assert_eq!(g.record.attempts.len(), 1);
    // A stale result (a call that outlived its claim: another token) is a
    // call: recorded once, on the cell it ran (its effort), although the
    // document's cell has since moved to another effort of the same model.
    let ran = call.request.effort;
    let other = if ran == Effort::High { Effort::Low } else { Effort::High };
    let mut moved = g.clone();
    moved.call.effort = other;
    db.conn
        .execute("UPDATE docs SET gen_record = ?1 WHERE seq = ?2 AND idx = ?3", rusqlite::params![gen_v2::encode(&moved).unwrap(), seq, it.idx])
        .unwrap();
    let cell_calls = |e: Effort| -> i64 {
        db.conn
            .query_row(
                "SELECT COALESCE(SUM(calls), 0) FROM gen_spend WHERE seq = ?1 AND model = ?2 AND effort = ?3",
                rusqlite::params![seq, it.generator, e.code()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let (ran_before, other_before) = (cell_calls(ran), cell_calls(other));
    let mut stale = res[0].clone();
    stale.claim += 1_000;
    let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &[stale], sim.now)).unwrap();
    assert_eq!(v["fenced"], 1);
    assert_eq!(cell_calls(ran), ran_before + 1);
    assert_eq!(cell_calls(other), other_before, "not charged on the document's current cell");
}

/// A Workers AI item never waits uncommitted behind a frontier call: the
/// lane claims each class on its own (`lanes::gen_frontier_model`), so the
/// Workers AI results commit within their short lease while a frontier
/// call is still out. Past that lease no other lane re-claims anything of
/// the batch, and every document is generated once. A claim mixing both
/// classes whose Workers AI results wait for the frontier call (the
/// hazard) would see them re-claimed.
#[test]
fn workers_ai_results_commit_within_their_lease_while_a_frontier_call_is_out() {
    let db = Sqlite::open();
    let (mut sim, seq, _) = v2_cohort(&db, "testnet-g2-e");
    sim.frozen = vec!["gen"];
    let drawn = || -> i64 { db.conn.query_row("SELECT COUNT(*) FROM docs WHERE seq = ?1 AND gen_record IS NOT NULL", [seq], |r| r.get(0)).unwrap() };
    for _ in 0..20 {
        if drawn() > 0 {
            break;
        }
        sim.tick();
    }
    assert_eq!(phase(&db, seq), "generating");
    let c2 = cfg();
    let plan: CohortPlan = serde_json::from_str(&db.conn.query_row("SELECT plan FROM cohorts WHERE seq = ?1", [seq], |r| r.get::<_, String>(0)).unwrap()).unwrap();
    let wai_lease = plan.fanout().unwrap().generation.lease_ms;
    let frontier_lease = plan.generation.as_ref().unwrap().frontier_lanes.lease_ms;
    assert!(wai_lease < frontier_lease);
    let (lane, generation): (i64, i64) =
        db.conn.query_row("SELECT lane, gen FROM lanes WHERE seq = ?1 AND kind = 'gen' ORDER BY lane LIMIT 1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let r = LaneRef { seq, kind: "gen".into(), lane, generation };
    let t0 = sim.now;
    let GenClaimReply::Wait { wants, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &BTreeMap::new(), t0)).unwrap() else { panic!() };
    let class = |f: bool| -> BTreeMap<String, u32> { wants.iter().filter(|(m, _)| lanes::gen_frontier_model(m) == f).map(|(m, n)| (m.clone(), (*n).min(2))).collect() };
    let (wai, frontier) = (class(false), class(true));
    assert!(!wai.is_empty() && !frontier.is_empty(), "{wants:?}");
    // The hazard, shown on the store: a mixed claim whose Workers AI
    // results are not committed is re-claimable once their lease passes.
    let mixed: BTreeMap<String, u32> = wai.iter().chain(frontier.iter()).map(|(m, n)| (m.clone(), *n)).collect();
    let GenClaimReply::Items { items: held, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &mixed, t0)).unwrap() else { panic!() };
    let late = t0 + wai_lease + 1;
    let GenClaimReply::Items { items: retaken, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "b", &wai, late)).unwrap() else { panic!() };
    let held_wai: BTreeSet<i64> = held.iter().filter(|it| !lanes::gen_frontier_model(&it.generator)).map(|it| it.idx).collect();
    assert!(retaken.iter().any(|it| held_wai.contains(&it.idx)), "an uncommitted Workers AI item is re-claimable past its lease");
    // Settle those claims (their results commit at once) before the lane's way.
    let out: Vec<GenOutcome> = retaken.iter().map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
    tx(&db, |s| lanes::gen_commit(s, &c2, &r, &out, late)).unwrap();
    let out: Vec<GenOutcome> = held.iter().filter(|it| lanes::gen_frontier_model(&it.generator)).map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
    tx(&db, |s| lanes::gen_commit(s, &c2, &r, &out, late)).unwrap();
    // The lane's way: one claim per class; the Workers AI class commits
    // when its calls return, the frontier class when its call returns.
    let t1 = late + 1;
    let GenClaimReply::Wait { wants, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &BTreeMap::new(), t1)).unwrap() else { panic!() };
    let class = |f: bool| -> BTreeMap<String, u32> { wants.iter().filter(|(m, _)| lanes::gen_frontier_model(m) == f).map(|(m, n)| (m.clone(), (*n).min(2))).collect() };
    let (wai, frontier) = (class(false), class(true));
    assert!(!wai.is_empty() && !frontier.is_empty(), "{wants:?}");
    let GenClaimReply::Items { items: fi, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &frontier, t1)).unwrap() else { panic!() };
    let GenClaimReply::Items { items: wi, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &wai, t1)).unwrap() else { panic!() };
    assert!(fi.iter().all(|it| lanes::gen_frontier_model(&it.generator)) && wi.iter().all(|it| !lanes::gen_frontier_model(&it.generator)));
    let wout: Vec<GenOutcome> = wi.iter().map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
    let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &wout, t1 + 1_000)).unwrap();
    assert_eq!(v["fenced"].as_u64().unwrap_or(0), 0);
    // Past the Workers AI lease, the frontier call still out: nothing of
    // either batch is claimable by another lane.
    let t2 = t1 + wai_lease + 1;
    let all: BTreeMap<String, u32> = wants.keys().map(|m| (m.clone(), 1_000)).collect();
    let again = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "b", &all, t2)).unwrap();
    let ours: BTreeSet<i64> = fi.iter().chain(wi.iter()).map(|it| it.idx).collect();
    if let GenClaimReply::Items { items, .. } = &again {
        assert!(items.iter().all(|it| !ours.contains(&it.idx)), "an item of the lane's batches was re-claimed");
        // Release b's claims (committed as answered).
        let out: Vec<GenOutcome> = items.iter().map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
        tx(&db, |s| lanes::gen_commit(s, &c2, &r, &out, t2)).unwrap();
    }
    // The frontier call returns within its own lease.
    assert!(t2 < t1 + frontier_lease);
    let fout: Vec<GenOutcome> = fi.iter().map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
    let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &fout, t2)).unwrap();
    assert_eq!(v["fenced"].as_u64().unwrap_or(0), 0);
    for idx in ours {
        let g = tx(&db, |s| gen_v2::doc_gen(s, seq, idx)).unwrap().unwrap();
        assert_eq!(g.record.attempts.len(), 1, "document {idx} generated once");
        let st: String = db.conn.query_row("SELECT status FROM docs WHERE seq = ?1 AND idx = ?2", rusqlite::params![seq, idx], |r| r.get(0)).unwrap();
        assert_eq!(st, "ready");
    }
}

/// The frontier class's grants: at most its share of the connection limit
/// in total, at most each model's claimable count, spread round-robin.
#[test]
fn frontier_grants_stay_within_the_class_share() {
    let wants: BTreeMap<String, u32> =
        [("@cf/meta/llama-3.2-3b-instruct", 9), ("claude-haiku-5-5", 1), ("gpt-6-luna", 10), ("gpt-6-astra", 4)].into_iter().map(|(m, n)| (m.to_owned(), n)).collect();
    for slots in 0..=8u32 {
        for start in 0..5 {
            let g = lanes::frontier_grants(&wants, slots, start);
            let total: u32 = g.values().sum();
            assert_eq!(total, slots.min(15), "{slots} {start}");
            assert!(g.keys().all(|m| lanes::gen_frontier_model(m)), "no Workers AI model");
            assert!(g.iter().all(|(m, n)| *n <= wants[m]));
            // Spread: no model gets two before every model with work has one.
            if slots >= 3 {
                assert!(g.values().all(|n| *n >= 1), "{g:?}");
            }
        }
    }
    // The starting model rotates with the round.
    let one: Vec<String> = (0..3).map(|s| lanes::frontier_grants(&wants, 1, s).into_keys().next().unwrap()).collect();
    assert_eq!(one.iter().collect::<BTreeSet<_>>().len(), 3);
    assert!(lanes::frontier_grants(&BTreeMap::new(), 3, 0).is_empty());
}

/// The cohort object's dispatch: the operator's vendor clear, under
/// `/gen/` like the lanes' claims and commits, is not diverted to the lane
/// routes (which have no arm for it); the lanes' paths are.
#[test]
fn the_vendor_clear_reaches_the_cohort_objects_operator_routes() {
    assert_eq!(lanes::GEN_VENDOR_CLEAR_PATH, "/gen/vendor-clear");
    assert!(!lanes::lane_route_path(lanes::GEN_VENDOR_CLEAR_PATH));
    for p in ["/gen/claim", "/gen/commit", "/lane/report", "/lane/summary", "/job/preds", "/job/objects"] {
        assert!(lanes::lane_route_path(p), "{p}");
    }
    for p in ["/plan", "/clear", "/hold", "/template", "/admission/x", "/gen"] {
        assert!(!lanes::lane_route_path(p), "{p}");
    }
    // The clear itself: idempotent, `false` for a vendor that is not paused.
    let db = Sqlite::open();
    tx(&db, |s| crate::sql::migrate(s, &crate::sql::EVAL_COHORT_SCHEMA)).unwrap();
    let v = gen_v2::pausable_vendor("openai").unwrap();
    assert!(!tx(&db, |s| gen_v2::resume_vendor(s, v)).unwrap());
    assert!(gen_v2::pausable_vendor("nobody").is_none());
}

/// A v2 lane's class ends combine into the lane's: superseded first, then
/// a terminal state, then the earliest re-arm; an idle class decides
/// nothing, and an idle lane re-arms at once. Only superseded and terminal
/// ends stop the other class.
#[test]
fn a_v2_lanes_class_ends_combine() {
    use lanes::{GenClassEnd as E, GenLaneEnd as L};
    let all = [E::Rearm(0), E::Rearm(7), E::Rearm(30_000), E::Superseded, E::Terminal("done"), E::Terminal("paused"), E::Idle];
    for a in all {
        for b in all {
            let c = E::combine(a, b);
            if !matches!((a, b), (E::Terminal(_), E::Terminal(_))) {
                assert_eq!(c, E::combine(b, a), "{a:?} {b:?}");
            }
            if a == E::Superseded || b == E::Superseded {
                assert_eq!(c, E::Superseded);
            } else if let E::Terminal(_) = a {
                assert_eq!(c, a);
            } else if let E::Terminal(_) = b {
                assert_eq!(c, b);
            } else {
                match (a, b) {
                    (E::Rearm(x), E::Rearm(y)) => assert_eq!(c, E::Rearm(x.min(y))),
                    (E::Rearm(x), E::Idle) | (E::Idle, E::Rearm(x)) => assert_eq!(c, E::Rearm(x)),
                    _ => assert_eq!(c, E::Idle),
                }
            }
        }
        assert_eq!(a.stops_lane(), matches!(a, E::Superseded | E::Terminal(_)));
    }
    assert_eq!(E::Idle.lane(), L::Rearm(0));
    assert_eq!(E::Rearm(9).lane(), L::Rearm(9));
    assert_eq!(E::Superseded.lane(), L::Superseded);
    assert_eq!(E::Terminal("paused").lane(), L::Terminal("paused"));
}

/// A v2 lane class's step on a `Wait` reply: it claims at once only for
/// work of its own class; while the other class works it waits on that
/// class (its commits or its end), at most the due time or the report
/// interval, instead of polling the cohort object.
#[test]
fn a_v2_lane_class_waits_on_the_other_class_rather_than_polling() {
    use lanes::{GEN_HOLD_MAX_MS, GEN_WAIT_MIN_MS, GenClassWait as W, gen_class_wait};
    let report = 20_000;
    let w = |pairs: &[(&str, u32)]| -> BTreeMap<String, u32> { pairs.iter().map(|(m, n)| ((*m).to_owned(), *n)).collect() };
    let wai = w(&[("@cf/meta/llama-3.2-3b-instruct", 3)]);
    let fr = w(&[("gpt-6-luna", 2)]);
    let none = BTreeMap::new();
    // Work of its own class: claim.
    assert_eq!(gen_class_wait(0, &wai, false, true, report), W::Claim);
    assert_eq!(gen_class_wait(0, &fr, true, false, report), W::Claim);
    // Only the other class's work while it runs: wait on it, up to the
    // report interval (no due time for this class).
    assert_eq!(gen_class_wait(0, &fr, false, true, report), W::AwaitOther { ms: report });
    assert_eq!(gen_class_wait(0, &wai, true, true, report), W::AwaitOther { ms: report });
    // Nothing claimable: wait on the other class up to the due time.
    assert_eq!(gen_class_wait(1_500, &none, true, true, report), W::AwaitOther { ms: 1_500 });
    assert_eq!(gen_class_wait(30_000, &none, false, true, report), W::AwaitOther { ms: report });
    // The other class has ended: hold a short wait, re-arm a long one.
    assert_eq!(gen_class_wait(GEN_WAIT_MIN_MS, &none, false, false, report), W::Hold { ms: GEN_WAIT_MIN_MS });
    assert_eq!(gen_class_wait(GEN_HOLD_MAX_MS, &none, false, false, report), W::Hold { ms: GEN_HOLD_MAX_MS });
    assert_eq!(gen_class_wait(GEN_HOLD_MAX_MS + 1, &none, true, false, report), W::Rearm { ms: GEN_HOLD_MAX_MS + 1 });
    // Work only for the other class, which has ended: this class is idle.
    assert_eq!(gen_class_wait(0, &fr, false, false, report), W::Idle);
    assert_eq!(gen_class_wait(0, &wai, true, false, report), W::Idle);
}

/// A claim with no grant (an idle lane class refreshing its wants) reads
/// neither generation object: it still answers while the objects are
/// unreadable, which a claim with a grant is not.
#[test]
fn an_empty_v2_claim_does_not_load_the_generation_objects() {
    let db = Sqlite::open();
    let (mut sim, seq, _) = v2_cohort(&db, "testnet-g2-h");
    sim.frozen = vec!["gen"];
    let drawn = || -> i64 { db.conn.query_row("SELECT COUNT(*) FROM docs WHERE seq = ?1 AND gen_record IS NOT NULL", [seq], |r| r.get(0)).unwrap() };
    for _ in 0..20 {
        if drawn() > 0 {
            break;
        }
        sim.tick();
    }
    assert_eq!(phase(&db, seq), "generating");
    let c2 = cfg();
    let (lane, generation): (i64, i64) =
        db.conn.query_row("SELECT lane, gen FROM lanes WHERE seq = ?1 AND kind = 'gen' ORDER BY lane LIMIT 1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let r = LaneRef { seq, kind: "gen".into(), lane, generation };
    let before = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &BTreeMap::new(), sim.now)).unwrap();
    let GenClaimReply::Wait { ms: 0, wants } = &before else { panic!("{before:?}") };
    db.conn.execute("UPDATE gen_objects SET body = 'unreadable' WHERE seq = ?1", [seq]).unwrap();
    let zero: BTreeMap<String, u32> = wants.keys().map(|m| (m.clone(), 0)).collect();
    for g in [BTreeMap::new(), zero] {
        assert_eq!(tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &g, sim.now)).unwrap(), before);
    }
    let one: BTreeMap<String, u32> = wants.keys().take(1).map(|m| (m.clone(), 1)).collect();
    assert!(tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &one, sim.now)).is_err());
}

/// A v2 call that commits after its cohort left `generating` (it outlived
/// its lease; another lane finished the item) applies nothing, but its
/// vendor spend is recorded once on the cell it ran and added to the
/// cohort's generation cost, until the cohort's spend is settled.
#[test]
fn a_late_v2_commit_outside_generating_records_its_spend_once() {
    let db = Sqlite::open();
    let (mut sim, seq, _) = v2_cohort(&db, "testnet-g2-i");
    sim.frozen = vec!["gen"];
    let drawn = || -> i64 { db.conn.query_row("SELECT COUNT(*) FROM docs WHERE seq = ?1 AND gen_record IS NOT NULL", [seq], |r| r.get(0)).unwrap() };
    for _ in 0..20 {
        if drawn() > 0 {
            break;
        }
        sim.tick();
    }
    assert_eq!(phase(&db, seq), "generating");
    let c2 = cfg();
    let (lane, generation): (i64, i64) =
        db.conn.query_row("SELECT lane, gen FROM lanes WHERE seq = ?1 AND kind = 'gen' ORDER BY lane LIMIT 1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let r = LaneRef { seq, kind: "gen".into(), lane, generation };
    let GenClaimReply::Wait { wants, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &BTreeMap::new(), sim.now)).unwrap() else { panic!() };
    let grants = lanes::frontier_grants(&wants, 2, 0);
    assert!(!grants.is_empty(), "{wants:?}");
    let GenClaimReply::Items { items, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &grants, sim.now)).unwrap() else { panic!() };
    let res: Vec<GenOutcome> = items.iter().map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
    let docs_before: Vec<(String, i64, String)> = items
        .iter()
        .map(|it| {
            db.conn
                .query_row("SELECT status, claim_gen, gen_record FROM docs WHERE seq = ?1 AND idx = ?2", rusqlite::params![seq, it.idx], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
        })
        .collect();
    // The cohort leaves generating before the calls commit.
    db.conn.execute("UPDATE cohorts SET phase = 'scoring' WHERE seq = ?1", [seq]).unwrap();
    let calls = || -> i64 { db.conn.query_row("SELECT COALESCE(SUM(calls), 0) FROM gen_spend WHERE seq = ?1", [seq], |r| r.get(0)).unwrap() };
    let cost = || -> i64 { db.conn.query_row("SELECT gen_cost_microusd FROM cohorts WHERE seq = ?1", [seq], |r| r.get(0)).unwrap() };
    let (calls0, cost0) = (calls(), cost());
    let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &res, sim.now)).unwrap();
    assert_eq!(v["late"], res.len() as i64);
    assert_eq!(v["accepted"], 0);
    assert_eq!(calls(), calls0 + res.len() as i64, "each late call is recorded once");
    let charged = v["cost_microusd"].as_i64().unwrap();
    assert!(charged > 0);
    assert_eq!(cost(), cost0 + charged);
    for (it, before) in items.iter().zip(&docs_before) {
        let now: (String, i64, String) = db
            .conn
            .query_row("SELECT status, claim_gen, gen_record FROM docs WHERE seq = ?1 AND idx = ?2", rusqlite::params![seq, it.idx], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(&now, before, "nothing applies to document {}", it.idx);
    }
    // A redelivery records nothing again.
    let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &res, sim.now)).unwrap();
    assert_eq!(v["fenced"], res.len() as i64);
    assert_eq!(calls(), calls0 + res.len() as i64);
    assert_eq!(cost(), cost0 + charged);
    // After the settle, a late commit is refused (the ledger charged the
    // cohort once).
    db.conn.execute("UPDATE cohorts SET settled_microusd = 0 WHERE seq = ?1", [seq]).unwrap();
    let mut other = res[0].clone();
    other.claim += 1_000;
    assert!(tx(&db, |s| lanes::gen_commit(s, &c2, &r, &[other], sim.now)).is_err());
    assert_eq!(calls(), calls0 + res.len() as i64);
}

/// A v2 lane's assignment keeps both classes' calls in flight within the
/// lane's connection limit.
#[test]
fn a_v2_lane_assignment_splits_the_connection_limit() {
    let db = Sqlite::open();
    let (mut sim, seq, _) = v2_cohort(&db, "testnet-g2-f");
    sim.frozen = vec!["gen"];
    let lanes_up = || -> i64 { db.conn.query_row("SELECT COUNT(*) FROM lanes WHERE seq = ?1 AND kind = 'gen'", [seq], |r| r.get(0)).unwrap() };
    for _ in 0..20 {
        if lanes_up() > 0 {
            break;
        }
        sim.tick();
    }
    let (lane, generation): (i64, i64) =
        db.conn.query_row("SELECT lane, gen FROM lanes WHERE seq = ?1 AND kind = 'gen' ORDER BY lane LIMIT 1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let r = LaneRef { seq, kind: "gen".into(), lane, generation };
    let a = tx(&db, |s| lanes::lane_report(s, &r, &LaneReport { state: "starting".into(), ..Default::default() }, sim.now)).unwrap();
    let max = crate::eval::plan::GEN_V2_LANE_BOUNDS.max_inflight;
    assert!(a.gen_frontier_inflight >= 1 && a.inflight >= 1, "{} {}", a.gen_frontier_inflight, a.inflight);
    assert!(a.gen_frontier_inflight + a.inflight <= max);
    assert_eq!(a.gen_frontier_lease_ms, 300_000);
}

/// Arena witnesses of generation v2's memory (`--features arena-witness`,
/// one test thread): with the manifest and the voice library together at
/// [`gen_v2::GEN_OBJECTS_BYTES`], a claim and a commit of each lane class
/// fit the cohort object's fetch footprint, and the draw's generation part
/// fits [`gen_v2::GEN_DRAW_BYTES`]. The measurement prints the parsed
/// context's peak per declared byte, the input of
/// `gen_v2::GEN_OBJECTS_PARSE_FACTOR`.
#[cfg(feature = "arena-witness")]
mod witness {
    use super::*;
    use crate::arena_routes::{COHORT_CONTROL_BYTES, R_COHORT_DRAW, R_COHORT_FETCH};
    use crate::arena_witness::{measure, witness};
    use slop_ninja_arena::{Mode, admit_current_block, set_mode};

    /// A v2 cohort generating, its objects at the bound (the library
    /// padded as `pad`); returns the declared sum.
    fn at_bound<'a>(db: &'a Sqlite, label: &str, pad: Pad) -> (Sim<'a>, i64, u64) {
        let (mut sim, seq, _) = v2_cohort_padded(db, label, Some(gen_v2::GEN_OBJECTS_BYTES), pad);
        sim.frozen = vec!["gen"];
        let lanes_up = || -> i64 { db.conn.query_row("SELECT COUNT(*) FROM lanes WHERE seq = ?1 AND kind = 'gen'", [seq], |r| r.get(0)).unwrap() };
        for _ in 0..20 {
            if lanes_up() > 0 {
                break;
            }
            sim.tick();
        }
        assert_eq!(phase(db, seq), "generating");
        let sum: i64 = db.conn.query_row("SELECT SUM(LENGTH(CAST(body AS BLOB))) FROM gen_objects WHERE seq = ?1", [seq], |r| r.get(0)).unwrap();
        let sum = sum as u64;
        assert!(sum <= gen_v2::GEN_OBJECTS_BYTES && sum + 1_024 >= gen_v2::GEN_OBJECTS_BYTES, "{sum}");
        (sim, seq, sum)
    }

    /// Both library shapes at the bound: entries of the longest text, and
    /// the shortest entries the schema allows (the most per-entry overhead
    /// per declared byte, the worst case under the spec).
    #[test]
    fn arena_witness_gen_objects_measure() {
        set_mode(Mode::Enforce);
        for (pad, label) in [(Pad::Long, "testnet-g2-w1"), (Pad::Short, "testnet-g2-w1s")] {
            let db = Sqlite::open();
            let (_sim, seq, sum) = at_bound(&db, label, pad);
            let c = tx(&db, |s| engine::cohort(s, seq)).unwrap();
            let (_, live) = measure(|| {
                let ctx = tx(&db, |s| gen_v2::Ctx::load(s, &c, &[5; 32])).unwrap();
                assert!(ctx.is_some());
            });
            let factor = live.div_ceil(sum as usize);
            eprintln!("arena witness: generation context ({pad:?} entries) of {sum} declared B peaks at {live} B live ({factor} B a byte)");
            assert!(factor as u64 <= gen_v2::GEN_OBJECTS_PARSE_FACTOR, "{pad:?}: measured {factor} B a byte");
        }
    }

    #[test]
    fn arena_witness_gen_claim_and_commit_fit_the_cohort_fetch_footprint() {
        set_mode(Mode::Enforce);
        for (pad, label) in [(Pad::Long, "testnet-g2-w2"), (Pad::Short, "testnet-g2-w2s")] {
            claim_and_commit_at_bound(pad, label);
        }
    }

    fn claim_and_commit_at_bound(pad: Pad, label: &str) {
        let db = Sqlite::open();
        let (sim, seq, _) = at_bound(&db, label, pad);
        let c2 = cfg();
        let (lane, generation): (i64, i64) =
            db.conn.query_row("SELECT lane, gen FROM lanes WHERE seq = ?1 AND kind = 'gen' ORDER BY lane LIMIT 1", [seq], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let r = LaneRef { seq, kind: "gen".into(), lane, generation };
        let GenClaimReply::Wait { wants, .. } = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &BTreeMap::new(), sim.now)).unwrap() else { panic!() };
        let max = crate::eval::plan::GEN_V2_LANE_BOUNDS.max_inflight;
        for frontier in [false, true] {
            // A whole class at the connection limit (one claim, then its commit).
            let grants: BTreeMap<String, u32> = if frontier {
                lanes::frontier_grants(&wants, max, 0)
            } else {
                let mut g = BTreeMap::new();
                let mut left = max;
                for (m, n) in wants.iter().filter(|(m, _)| !lanes::gen_frontier_model(m)) {
                    let k = (*n).min(left);
                    left -= k;
                    if k > 0 {
                        g.insert(m.clone(), k);
                    }
                }
                g
            };
            assert!(!grants.is_empty());
            // The reply leaves the route as bytes, copied into a buffer
            // allocated before the scope (nothing of the scope outlives it).
            let mut reply_bytes: Vec<u8> = Vec::with_capacity(COHORT_CONTROL_BYTES);
            witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
                let reply = tx(&db, |s| lanes::gen_claim(s, &c2, &r, "a", &grants, sim.now)).unwrap();
                let GenClaimReply::Items { items, .. } = reply else { panic!() };
                let b = serde_json::to_vec(&items).unwrap();
                assert!(b.len() <= reply_bytes.capacity());
                reply_bytes.extend_from_slice(&b);
            });
            let items: Vec<engine::GenItem> = serde_json::from_slice(&reply_bytes).unwrap();
            let out: Vec<GenOutcome> = items.iter().map(|it| GenOutcome::from_reply(it, fake_vendor(it, it.call.as_deref().unwrap()))).collect();
            let body = serde_json::to_vec(&out).unwrap();
            drop(out);
            witness(R_COHORT_FETCH.route(), COHORT_CONTROL_BYTES, || {
                assert_eq!(admit_current_block(body.len(), 1), Ok(true));
                let mut b = Vec::with_capacity(body.len());
                b.extend_from_slice(&body);
                let res: Vec<GenOutcome> = serde_json::from_slice(&b).unwrap();
                let v = tx(&db, |s| lanes::gen_commit(s, &c2, &r, &res, sim.now)).unwrap();
                v.to_string().len()
            });
        }
    }

    #[test]
    fn arena_witness_the_draw_holds_the_generation_objects_within_its_addition() {
        set_mode(Mode::Enforce);
        for (pad, label) in [(Pad::Long, "testnet-g2-w3"), (Pad::Short, "testnet-g2-w3s")] {
            draw_at_bound(pad, label);
        }
    }

    fn draw_at_bound(pad: Pad, label: &str) {
        let db = Sqlite::open();
        let (sim, seq, _) = at_bound(&db, label, pad);
        let c = tx(&db, |s| engine::cohort(s, seq)).unwrap();
        let g = c.plan.generation.clone().unwrap();
        let mb = sim.eval[&g.manifest.key].clone();
        let m = Manifest::parse_bound(&mb, &g.manifest).unwrap();
        let lb = sim.eval[&m.voice_library.key].clone();
        drop(m);
        // A fresh store: the draw's own transaction keeps the objects.
        let fresh = Sqlite::open();
        tx(&fresh, |s| crate::sql::migrate(s, &crate::sql::EVAL_COHORT_SCHEMA)).unwrap();
        witness(R_COHORT_DRAW, gen_v2::GEN_DRAW_BYTES, || {
            // The Worker's two reads (exact blocks) and its manifest parse.
            assert_eq!(admit_current_block(mb.len(), 1), Ok(true));
            let mut mcopy = Vec::with_capacity(mb.len());
            mcopy.extend_from_slice(&mb);
            let m = Manifest::parse_bound(&mcopy, &g.manifest).unwrap();
            assert!(gen_v2::storable(&g.manifest, Some(&gen_v2::library_ref(&m))));
            assert_eq!(admit_current_block(lb.len(), 1), Ok(true));
            let mut lcopy = Vec::with_capacity(lb.len());
            lcopy.extend_from_slice(&lb);
            let objects = gen_v2::DrawObjects { manifest: mcopy, library: lcopy };
            let d = tx(&fresh, |s| gen_v2::Draw::begin(s, seq, &g, &objects, &[5; 32])).unwrap();
            d.ctx.library.entries.len()
        });
    }
}

/// A throttled item waits for the backoff or the provider's hint, but never
/// past the plan's throttle bound: an hour-long hint reaches the outage
/// path (and the fallback) at the bound, not after the full hint.
#[test]
fn throttle_wait_never_passes_the_bound() {
    let bound = 900_000;
    // Within the bound, a short hint wins over a shorter backoff.
    let w = crate::eval::engine::throttle_wait_ms(3, 0, Some(30_000), 1_000, bound, 1_000);
    assert_eq!(w, 30_000);
    // No hint: the backoff.
    assert_eq!(crate::eval::engine::throttle_wait_ms(3, 0, None, 1_000, bound, 1_000), crate::eval::engine::throttle_backoff_ms(3, 0));
    // An hour-long hint is cut to just past the bound.
    let w = crate::eval::engine::throttle_wait_ms(3, 0, Some(3_600_000), 1_000, bound, 101_000);
    assert_eq!(w, bound - 100_000 + 1);
    // At the bound the next call is immediate (1 ms), then the next throttle is past it.
    assert_eq!(crate::eval::engine::throttle_wait_ms(3, 4, Some(600_000), 1_000, bound, 1_000 + bound), 1);
}

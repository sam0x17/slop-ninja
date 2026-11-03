//! Offline tests for `r2_test2_carve_v3` (build B9): synthetic keys, synthetic stock,
//! synthetic pools and synthetic freeze logs only. No private file, model or detector
//! is touched.
use crate::capacity::{self, PPM, Params, Reuse};
use crate::carve_v3::{self, ApplyArgs, Candidate, CarveParams, DomainStock, FreezeLog, Inputs, ReleaseArgs, Sources, Target};
use crate::pool_v2;
use crate::sha256_hex;
use crate::stock::{self, ActiveReference, FileRef, StockAllowlist};
use crate::tests_v2::{Fixture, Repo, gref, params, title};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;

const KEY_A: [u8; 32] = [7; 32];
const KEY_B: [u8; 32] = [9; 32];

fn mock(n: u32) -> Params {
    Params::new(n, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 0)
}

fn plan10() -> Params {
    Params::new(10, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 0)
}

/// A synthetic domain of `groups` one-document groups at the given yields.
fn dom(key: &[u8; 32], name: &str, groups: usize, mass: u64, carved: bool, r: f64, rl: f64) -> DomainStock {
    let mut c: Vec<Candidate> = (0..groups)
        .map(|i| {
            let k = format!("{name}:g{i}");
            Candidate { rank: carve_v3::rank(key, name, &k), group_key: k, units: 1, udocs: 1_000_000 }
        })
        .collect();
    c.sort_by_key(|x| x.rank);
    DomainStock { test_mass: mass, realization: r, realization_lower: rl, udocs: groups as u64 * 1_000_000, candidates: if carved { c } else { Vec::new() } }
}

/// Synthetic inputs whose manifest weights are the stock's own waterfill.
fn inputs_at(key: &[u8; 32], spec: &[(&str, usize, u64, bool)], horizon: u32, r: f64, rl: f64) -> Inputs {
    let domains: BTreeMap<String, DomainStock> = spec.iter().map(|(n, g, m, c)| (n.to_string(), dom(key, n, *g, *m, *c, r, rl))).collect();
    let recorded_lower: BTreeMap<String, u64> = domains.iter().map(|(d, s)| (d.clone(), carve_v3::lower(s.udocs, s.realization_lower))).collect();
    let base = domains.iter().map(|(d, s)| (d.clone(), s.test_mass)).collect();
    let w = capacity::waterfill(&base, &recorded_lower, &plan10(), horizon).unwrap().weights_ppm;
    Inputs { domains, plan: plan10(), cohorts_per_day: 1, horizon, manifest_weights: w, recorded_lower, consumed_test: BTreeSet::new() }
}

fn inputs(key: &[u8; 32], spec: &[(&str, usize, u64, bool)], horizon: u32) -> Inputs {
    inputs_at(key, spec, horizon, 1.0, 1.0)
}

fn cparams(targets: &[(&str, u64, u64)], b_res: u32, horizon: u32, ar8_min_days: u32) -> CarveParams {
    CarveParams {
        targets: targets.iter().map(|(d, docs, g)| (d.to_string(), Target { lower_documents: *docs, min_groups: *g })).collect(),
        b_res,
        mock: mock(10),
        horizon,
        ar8_min_days,
        iterations: 2,
        stage3_raised_domain: targets[0].0.to_string(),
    }
}

/// big and tight are carved; tight binds at the horizon; other is not carved.
fn two_domains(key: &[u8; 32], tight: usize) -> Inputs {
    inputs(key, &[("big", 400, 50, true), ("tight", tight, 50, true), ("other", 100, 10, false)], 10)
}

fn two_params() -> CarveParams {
    cparams(&[("big", 10, 3), ("tight", 3, 3)], 3, 10, 5)
}

// ------------------------------------------------------------------ protocol values

#[test]
fn the_protocol_values_are_v6_and_v10() {
    let p = CarveParams::protocol_a2();
    let docs = |d: &str| p.targets[d].lower_documents;
    assert_eq!((docs("legal_opinions"), docs("wikipedia"), docs("fineweb")), (450, 400, 220));
    for d in ["reddit", "ubuntu_irc", "wikipedia_talk", "fineweb_blogs"] {
        assert_eq!(docs(d), 300, "{d}");
    }
    // 150 humans in 15 groups at the inherited 1.3 buffer, in lower-bound documents
    // (amendment 3 item 3.1): 195 documents, and 20 groups in every carved domain.
    for d in ["wikisource", "plos", "instructional", "peer_review"] {
        assert_eq!(docs(d), 195, "{d}");
    }
    assert_eq!(p.targets.len(), 11);
    assert!(p.targets.values().all(|t| t.min_groups == 20));
    // B_res = ceil((2.80 x 0.19 / 0.070)^2) = ceil(57.76) = 58.
    assert_eq!(p.b_res, (2.80f64 * 0.19 / 0.070).powi(2).ceil() as u32);
    assert_eq!((p.mock.n, p.mock.mix, p.mock.reuse, p.mock.probe_docs), (1000, [20, 55, 25], Reuse::Ledger { max_uses: 4, min_gap: 7 }, 0));
    assert_eq!((p.horizon, p.ar8_min_days, p.iterations), (365, 90, 2));
    assert_eq!(p.stage3_raised_domain, "legal_opinions");
}

#[test]
fn ledger_demand_takes_550_fresh_humans_for_batches_one_to_seven_then_200() {
    let p = CarveParams::protocol_a2();
    let unit: BTreeMap<String, u64> = [("u".to_string(), PPM)].into();
    for b in 1..=58u32 {
        let want = 550 * u64::from(b.min(7)) + 200 * u64::from(b.saturating_sub(7));
        assert_eq!(capacity::demand(&unit, &p.mock, b).unwrap()["u"], want, "b {b}");
    }
    assert_eq!(capacity::demand(&unit, &p.mock, 58).unwrap()["u"], 14_050);
    // The same on a finite two-domain stock, batch by batch.
    let stock: BTreeMap<String, u64> = [("a", 100_000), ("b", 50_000)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let w: BTreeMap<String, u64> = [("a", 666_667), ("b", 333_333)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let o = capacity::simulate(&stock, &w, &p.mock).unwrap();
    assert_eq!((o.fresh_taken["cohort_1"], o.fresh_taken["cohort_8"]), (550, 200));
    // The renormalised split follows the weights within a document per domain.
    let d = capacity::demand(&w, &p.mock, 58).unwrap();
    assert_eq!(d.values().sum::<u64>(), 14_050);
    assert!((d["a"] as f64 - 14_050.0 * 2.0 / 3.0).abs() <= 2.0, "{d:?}");
}

#[test]
fn hmac_matches_rfc_4231_and_the_order_is_keyed() {
    let h = |k: &[u8], m: &[u8]| hex::encode(carve_v3::hmac_sha256(k, m));
    assert_eq!(h(&[0x0b; 20], b"Hi There"), "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7");
    assert_eq!(h(b"Jefe", b"what do ya want for nothing?"), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    assert_eq!(h(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First"),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
    // The rank is HMAC over the label, the domain and the group key (amendment 3 item 3.2).
    let r = carve_v3::rank(&KEY_A, "wiki", "wikipedia:page:1");
    assert_eq!(r, carve_v3::hmac_sha256(&KEY_A, b"slop_ninja_a2_test2_carve_v3:wiki:wikipedia:page:1"));
    assert_ne!(r, carve_v3::rank(&KEY_B, "wiki", "wikipedia:page:1"));
    assert_ne!(r, carve_v3::rank(&KEY_A, "other", "wikipedia:page:1"));
}

#[test]
fn utc_stamps_are_rfc_3339() {
    assert_eq!(carve_v3::utc_from_unix(0), "1970-01-01T00:00:00Z");
    assert_eq!(carve_v3::utc_from_unix(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(carve_v3::utc_from_unix(1_704_067_199), "2023-12-31T23:59:59Z");
}

// ------------------------------------------------------------------ the computation

#[test]
fn a_synthetic_key_gives_a_deterministic_carve_and_another_key_another() {
    let p = two_params();
    let a1 = carve_v3::compute(&two_domains(&KEY_A, 20), &p).unwrap();
    let a2 = carve_v3::compute(&two_domains(&KEY_A, 20), &p).unwrap();
    assert_eq!(a1.result, a2.result);
    assert_eq!(a1.groups, a2.groups);
    let b = carve_v3::compute(&two_domains(&KEY_B, 20), &p).unwrap();
    let keys = |c: &carve_v3::Computed| c.groups.iter().map(|g| g.group_key.clone()).collect::<BTreeSet<_>>();
    assert_ne!(keys(&a1), keys(&b), "a different key draws other groups");
    // Sizes depend on the stock and the weights, not on the key.
    assert_eq!(a1.groups.len(), b.groups.len());
    assert_eq!(a1.result["w_a8"], b.result["w_a8"]);
    // The carve is a keyed prefix: Test-2 T first, then the reserve, in ascending rank order.
    let inp = two_domains(&KEY_A, 20);
    for d in ["big", "tight"] {
        let c = &inp.domains[d].candidates;
        assert!(c.windows(2).all(|w| w[0].rank < w[1].rank));
        let mine: Vec<&carve_v3::CarveGroup> = a1.groups.iter().filter(|g| g.domain == d).collect();
        for (i, g) in mine.iter().enumerate() {
            assert_eq!(g.order, i as u64);
            assert_eq!(g.group_key, c[i].group_key);
        }
        let first_reserve = mine.iter().position(|g| g.role == carve_v3::ROLE_RESERVE).unwrap_or(mine.len());
        assert!(mine[..first_reserve].iter().all(|g| g.role == carve_v3::ROLE_TEST2));
        assert!(mine[first_reserve..].iter().all(|g| g.role == carve_v3::ROLE_RESERVE));
    }
    // Nothing key-derived is in the record.
    let s = serde_json::to_string(&a1.result).unwrap();
    assert!(!s.contains(&hex::encode(KEY_A)) && !s.contains("\"rank\""));
}

#[test]
fn targets_count_lower_bound_documents_when_the_yield_is_below_one() {
    // One-document groups at a point yield of 0.8 and a lower bound of 0.5: 6 lower-bound
    // documents need 12 groups (6 expected documents would need 6, 6 at the point yield 8).
    let inp = inputs_at(&KEY_A, &[("low", 200, 50, true), ("other", 400, 50, false)], 10, 0.8, 0.5);
    let c = carve_v3::compute(&inp, &cparams(&[("low", 6, 2)], 1, 10, 1)).unwrap();
    let t2: Vec<_> = c.groups.iter().filter(|g| g.role == carve_v3::ROLE_TEST2).collect();
    assert_eq!(t2.len(), 12, "{}", c.result["iterations"][0]["domains"]["low"]);
    let rec = &c.result["iterations"][0]["domains"]["low"]["test2_t"];
    assert_eq!((rec["lower_documents"].as_u64(), rec["expected_documents"].as_f64()), (Some(6), Some(12.0)));
    // The mock demand is in the same unit: the carve plus reserve holds it at the lower bound.
    let whole = &c.result["iterations"][0]["domains"]["low"];
    assert!(whole["carve_plus_reserve"]["lower_documents"].as_u64().unwrap() >= whole["sizing_mock_demand"].as_u64().unwrap());
    let held: u64 = c.groups.iter().map(|g| g.expected_udocs).sum();
    assert_eq!(carve_v3::lower(held, 0.5), whole["carve_plus_reserve"]["lower_documents"].as_u64().unwrap());
    // The group floor binds when it needs more groups than the documents do.
    let c = carve_v3::compute(&inp, &cparams(&[("low", 6, 15)], 1, 10, 1)).unwrap();
    assert_eq!(c.groups.iter().filter(|g| g.role == carve_v3::ROLE_TEST2).count(), 15);
}

#[test]
fn the_reserve_covers_w_a8_by_a_second_iteration_and_the_record_holds_the_tv_report() {
    let inp = two_domains(&KEY_A, 20);
    let p = two_params();
    let c = carve_v3::compute(&inp, &p).unwrap();
    let r = &c.result;
    assert!(c.pass, "{}", r["verdict"]);
    // tight binds at the horizon under the manifest; big does not.
    assert_eq!(r["manifest_in_force"]["binding_at_h"], json!(["tight"]));
    assert_eq!(r["surplus_bound"]["big"]["holds"], true);
    assert_eq!(r["surplus_bound"]["tight"]["binds_at_h"], true);
    // The re-waterfill reduces tight, so big's share of the mock rises above its first
    // reserve: the second iteration extends big's reserve and is recorded.
    let its = r["iterations"].as_array().unwrap();
    assert_eq!(its.len(), 2, "{}", r["iterations"]);
    assert_eq!(its[0]["reserve_covers_w_a8_demand"], false);
    assert_eq!(its[0]["domains"]["big"]["covers_w_a8_demand"], false);
    assert_eq!(its[1]["reserve_covers_w_a8_demand"], true);
    assert!(c.groups.iter().any(|g| g.domain == "big" && g.iteration == 2));
    assert!(c.groups.iter().all(|g| g.domain != "tight" || g.iteration == 1));
    let w_a8: BTreeMap<String, u64> = serde_json::from_value(r["w_a8"]["weights_ppm"].clone()).unwrap();
    assert!(w_a8["tight"] < inp.manifest_weights["tight"], "reduced domains' weights only fall");
    // Coverage at W_A8, checked independently.
    let carved: BTreeSet<String> = ["big".to_string(), "tight".to_string()].into();
    let sub: BTreeMap<String, f64> = carved.iter().map(|d| (d.clone(), w_a8[d] as f64)).collect();
    let need = capacity::demand(&capacity::to_ppm(&sub), &p.mock, p.b_res).unwrap();
    for d in &carved {
        let held: u64 = c.groups.iter().filter(|g| &g.domain == d).map(|g| g.expected_udocs).sum();
        assert!(carve_v3::lower(held, 1.0) >= need[d], "{d}");
    }
    // Test-2 T holds its target and floor.
    for (d, t) in &p.targets {
        let t2: Vec<_> = c.groups.iter().filter(|g| &g.domain == d && g.role == carve_v3::ROLE_TEST2).collect();
        assert!(t2.len() as u64 >= t.min_groups && t2.len() as u64 >= t.lower_documents, "{d}");
    }
    // AR-8 and TV.
    assert_eq!(r["ar8"]["pass"], true);
    assert!(r["ar8"]["days_before_first_bind_lower_yield"].as_f64().unwrap() >= 10.0);
    let tv = &r["tv"]["full_carve_and_reserve"];
    assert_eq!(tv["tv_from_manifest_ppm"], carve_v3::tv_ppm(&inp.manifest_weights, &w_a8));
    assert!(tv["tv_from_manifest_ppm"].as_u64().unwrap() > 0);
    assert!(r["tv"]["test2_t_carve_alone"]["tv_from_manifest_ppm"].as_u64().unwrap() <= tv["tv_from_manifest_ppm"].as_u64().unwrap());
    assert_eq!(r["failure"], Value::Null);
    // The exclusion section names the groups' digest and W_A8.
    assert_eq!(r["exclusion"]["weights_ppm"], json!(w_a8));
    assert_eq!(r["exclusion"]["rows"], c.groups.len());
}

#[test]
fn ar8_failure_reports_the_binding_domains_and_both_largest_b_res_values() {
    // tight is small: the reserve at B_res = 20 exhausts it, which retires it.
    let inp = two_domains(&KEY_A, 9);
    let p = cparams(&[("big", 10, 3), ("tight", 3, 3)], 20, 10, 5);
    let c = carve_v3::compute(&inp, &p).unwrap();
    let r = &c.result;
    assert!(!c.pass);
    let reasons = r["verdict"]["reasons"].to_string();
    assert!(reasons.contains("AR-8 fails"), "{reasons}");
    let f = &r["failure"];
    assert_eq!(f["applied"], false);
    assert!(f["binding_domains"]["newly_retired"].as_array().unwrap().iter().any(|d| d == "tight"), "{f}");
    let b = f["largest_b_res_prime_with_ar8_passing"].as_u64().expect("a B_res' with AR-8 passing") as u32;
    assert!(b < 20, "{f}");
    // tight's Test-2 T alone exceeds its surplus here, so no B_res' passes every
    // criterion: that value is null and the search runs down to 1.
    assert_eq!(f["largest_b_res_prime_passing_a8"], Value::Null, "{f}");
    let tried = f["search"].as_array().unwrap();
    assert_eq!(tried.len(), 20);
    assert_eq!((tried[0]["b_res"].as_u64(), tried[19]["b_res"].as_u64()), (Some(20), Some(1)));
    for t in tried {
        assert_eq!(t["a8_pass"], false, "{t}");
        if t["b_res"].as_u64().unwrap() > u64::from(b) {
            assert_eq!(t["ar8_pass"], false, "{t}");
        }
    }
    // The value is the full procedure's at that B_res' (both iterations included).
    let at = |b: u32| carve_v3::compute(&inp, &CarveParams { b_res: b, ..p.clone() }).unwrap();
    assert_eq!(at(b).result["ar8"]["pass"], true);
    assert_eq!(at(b + 1).result["ar8"]["pass"], false);
}

#[test]
fn a_failure_other_than_ar8_reports_b_res_for_ar8_and_a_smaller_value_for_a8() {
    // m does not bind at H; its reserve at B_res = 7 exceeds its surplus there, while AR-8 passes.
    let inp = inputs(&KEY_A, &[("a", 400, 50, true), ("m", 14, 10, true)], 10);
    let p = cparams(&[("a", 3, 3), ("m", 1, 1)], 7, 10, 5);
    let c = carve_v3::compute(&inp, &p).unwrap();
    let r = &c.result;
    assert!(!c.pass, "{}", r["verdict"]);
    assert!(r["verdict"]["reasons"].to_string().contains("exceeds its surplus"), "{}", r["verdict"]);
    assert_eq!(r["ar8"]["pass"], true, "{}", r["ar8"]);
    let f = &r["failure"];
    assert_eq!(f["largest_b_res_prime_with_ar8_passing"], 7, "item 1.4's literal statistic is B_res itself here");
    let a8 = f["largest_b_res_prime_passing_a8"].as_u64().expect("a smaller reserve passes") as u32;
    assert!(a8 < 7, "{f}");
    let at = |b: u32| carve_v3::compute(&inp, &CarveParams { b_res: b, ..p.clone() }).unwrap();
    assert!(at(a8).pass);
    assert!(!at(a8 + 1).pass);
}

#[test]
fn a_carve_beyond_a_non_binding_domains_surplus_fails_the_bound() {
    // m does not bind at H (its demand is below its stock), but its target exceeds the surplus.
    let inp = inputs(&KEY_A, &[("a", 400, 50, true), ("m", 14, 10, true)], 10);
    let p = cparams(&[("a", 3, 3), ("m", 8, 3)], 1, 10, 5);
    let c = carve_v3::compute(&inp, &p).unwrap();
    let s = &c.result["surplus_bound"]["m"];
    assert_eq!(s["binds_at_h"], false, "{s}");
    assert_eq!(s["holds"], false, "{s}");
    assert!(s["carve_plus_reserve"].as_i64().unwrap() > s["surplus"].as_i64().unwrap());
    assert!(!c.pass);
    assert!(c.result["verdict"]["reasons"].to_string().contains("exceeds its surplus"));
    // No smaller reserve helps when the target alone exceeds the surplus.
    assert_eq!(c.result["failure"]["largest_b_res_prime_passing_a8"], Value::Null);
    // Within the surplus it holds.
    let ok = carve_v3::compute(&inp, &cparams(&[("a", 3, 3), ("m", 3, 3)], 1, 10, 5)).unwrap();
    assert_eq!(ok.result["surplus_bound"]["m"]["holds"], true);
}

#[test]
fn a_carve_holding_a_consumed_test_group_is_refused() {
    let mut inp = two_domains(&KEY_A, 20);
    let first = inp.domains["big"].candidates[0].group_key.clone();
    inp.consumed_test.insert(first);
    let e = format!("{:#}", carve_v3::compute(&inp, &two_params()).err().expect("refused"));
    assert!(e.contains("consumed Test") && e.contains("set (i)"), "{e}");
    // A consumed group the carve does not reach is no hit.
    let mut inp = two_domains(&KEY_A, 20);
    let last = inp.domains["big"].candidates.last().unwrap().group_key.clone();
    inp.consumed_test.insert(last);
    assert!(carve_v3::compute(&inp, &two_params()).unwrap().pass);
}

#[test]
fn the_manifest_must_reproduce_before_anything_is_carved() {
    let mut inp = two_domains(&KEY_A, 20);
    let k = inp.manifest_weights.keys().next().unwrap().clone();
    *inp.manifest_weights.get_mut(&k).unwrap() += 1;
    let e = format!("{:#}", carve_v3::compute(&inp, &two_params()).err().unwrap());
    assert!(e.contains("do not reproduce"), "{e}");
    let mut inp = two_domains(&KEY_A, 20);
    *inp.recorded_lower.get_mut("big").unwrap() += 1;
    assert!(format!("{:#}", carve_v3::compute(&inp, &two_params()).err().unwrap()).contains("the groups give"));
    let inp = two_domains(&KEY_A, 20);
    let e = format!("{:#}", carve_v3::compute(&inp, &CarveParams { horizon: 9, ..two_params() }).err().unwrap());
    assert!(e.contains("protocol's H"), "{e}");
}

// ------------------------------------------------------------------ the freeze log

fn ser(v: &Value, pretty: bool) -> Vec<u8> {
    if !pretty {
        return serde_json::to_vec(v).unwrap();
    }
    let mut out = Vec::new();
    let mut s = serde_json::Serializer::with_formatter(&mut out, serde_json::ser::PrettyFormatter::with_indent(b" "));
    v.serialize(&mut s).unwrap();
    out.push(b'\n');
    out
}

/// A hash-chained freeze log: the first entry unchained, every later one with
/// prev_sha256 = the digest of the file just before its append.
fn chained(entries: &[Value], pretty: bool) -> Vec<u8> {
    let mut log: Vec<Value> = Vec::new();
    let mut bytes = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let mut e = e.clone();
        if i > 0 {
            e["prev_sha256"] = json!(sha256_hex(&bytes));
        }
        log.push(e);
        // "log" sorts before "schema", so the log is not the file's last field.
        bytes = ser(&json!({"schema": "synthetic_freeze_v1", "log": log}), pretty);
    }
    bytes
}

fn a6(key: &[u8]) -> Value {
    json!({"utc": "2026-11-01T01:17:11Z", "event": "A6: key commitments",
        "commitments_sha256": {"K_test2": sha256_hex(key), "K_cc": sha256_hex(b"synthetic cc"), "K_mock": sha256_hex(b"synthetic mock")}})
}

fn base_log(key: &[u8]) -> Vec<Value> {
    vec![
        json!({"utc": "2026-10-31T23:59:22Z", "event": "freeze", "protocol_sha256": sha256_hex(b"synthetic protocol")}),
        json!({"utc": "2026-11-01T00:46:03Z", "event": "amendment 1: the log is hash-chained from this entry"}),
        a6(key),
    ]
}

fn decisions(u: &[(&str, &str)]) -> Value {
    let m: serde_json::Map<String, Value> = u.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    json!({"utc": "2026-11-01T03:00:00Z", "event": "amendment: user decisions", "user_decisions": m})
}

fn all_confirmed() -> Value {
    decisions(&[("U1", "confirmed"), ("U2", "confirmed"), ("U16", "confirmed"), ("U17", "confirmed")])
}

fn drafted(sha: &str) -> Value {
    json!({"utc": "2026-11-01T02:00:00Z", "event": "A8 computed: hashed draft of CARVE-A2.json (S0-5)", "carve_draft_sha256": sha})
}

fn confirmed(sha: &str) -> Vec<Value> {
    vec![drafted(sha), all_confirmed()]
}

#[test]
fn the_freeze_log_chain_is_verified_on_the_files_own_bytes() {
    let entries = [base_log(&KEY_A), confirmed(&sha256_hex(b"d"))].concat();
    for pretty in [false, true] {
        let b = chained(&entries, pretty);
        let log = FreezeLog::parse(&b).unwrap();
        assert_eq!((log.entries.len(), log.chained_from), (5, 1));
        assert_eq!(log.k_test2().unwrap().1, sha256_hex(&KEY_A));
        assert_eq!(log.decisions().unwrap()["U17"].0, "confirmed");
        // An earlier entry edited after later appends breaks the chain.
        let s = String::from_utf8(b.clone()).unwrap();
        let edited = s.replacen("A6: key commitments", "A6: key commitmentz", 1);
        let e = format!("{:#}", FreezeLog::parse(edited.as_bytes()).err().expect("refused"));
        assert!(e.contains("does not chain"), "{e}");
    }
    // An entry without prev_sha256 after the chain began is refused.
    let mut v: Value = serde_json::from_slice(&chained(&entries, false)).unwrap();
    v["log"].as_array_mut().unwrap().push(json!({"utc": "2026-11-01T04:00:00Z", "event": "unchained"}));
    let e = format!("{:#}", FreezeLog::parse(&serde_json::to_vec(&v).unwrap()).err().unwrap());
    assert!(e.contains("no prev_sha256 after the chain began"), "{e}");
    // A log that never chains, or chains from its first entry, is refused.
    let flat = serde_json::to_vec(&json!({"log": base_log(&KEY_A)})).unwrap();
    assert!(format!("{:#}", FreezeLog::parse(&flat).err().unwrap()).contains("not hash-chained"));
    let mut first = base_log(&KEY_A);
    first[0]["prev_sha256"] = json!(sha256_hex(b"x"));
    let e = format!("{:#}", FreezeLog::parse(&serde_json::to_vec(&json!({"log": first})).unwrap()).err().unwrap());
    assert!(e.contains("first entry"), "{e}");
    // Decisions are confirmed or overridden, the last one logged governing.
    let b = chained(&[base_log(&KEY_A), vec![decisions(&[("U1", "confirmed")]), decisions(&[("U1", "overridden")])]].concat(), false);
    assert_eq!(FreezeLog::parse(&b).unwrap().decisions().unwrap()["U1"].0, "overridden");
    let b = chained(&[base_log(&KEY_A), vec![decisions(&[("U1", "yes")])]].concat(), false);
    assert!(format!("{:#}", FreezeLog::parse(&b).unwrap().decisions().unwrap_err()).contains("confirmed\" or \"overridden"));
    // Exactly one A6 entry.
    let b = chained(&[base_log(&KEY_A), vec![a6(&KEY_B)]].concat(), false);
    assert!(format!("{:#}", FreezeLog::parse(&b).unwrap().k_test2().unwrap_err()).contains("exactly one A6 entry"));
}

// ------------------------------------------------------------------ files and keys

fn write_key(repo: &Repo, rel: &str, key: &[u8]) {
    let p = repo.root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::set_permissions(p.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&p, key).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn the_key_file_is_owner_only_and_matches_its_commitment() {
    let repo = Repo::new("carve-key");
    write_key(&repo, "data/keys/k.bin", &KEY_A);
    let c = sha256_hex(&KEY_A);
    assert_eq!(carve_v3::read_key(&repo.root, "data/keys/k.bin", &c).unwrap(), KEY_A);
    let e = format!("{:#}", carve_v3::read_key(&repo.root, "data/keys/k.bin", &sha256_hex(&KEY_B)).unwrap_err());
    assert!(e.contains("does not match its A6 commitment") && !e.contains(&hex::encode(KEY_A)), "{e}");
    fs::set_permissions(repo.root.join("data/keys/k.bin"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(format!("{:#}", carve_v3::read_key(&repo.root, "data/keys/k.bin", &c).unwrap_err()).contains("mode 600"));
    fs::set_permissions(repo.root.join("data/keys/k.bin"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(repo.root.join("data/keys"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(format!("{:#}", carve_v3::read_key(&repo.root, "data/keys/k.bin", &c).unwrap_err()).contains("mode 700"));
    write_key(&repo, "data/keys/short.bin", &KEY_A[..31]);
    assert!(format!("{:#}", carve_v3::read_key(&repo.root, "data/keys/short.bin", &sha256_hex(&KEY_A[..31])).unwrap_err()).contains("exactly 32 bytes"));
}

/// A synthetic pool v2 built by the real pool builder, its allow-list, a key, the
/// protocol split set, a base freeze log, and the parameters at which its carve passes.
struct Pool {
    f: Fixture,
    src: Sources,
    p: CarveParams,
}

const BASE_LOG: &str = "data/r2/log-base.json";
const DRAFT: &str = "data/r2/a8/CARVE-A2.draft.json";

fn pool_with(tag: &str, b_res: u32, raised: &str) -> Pool {
    let mut f = Fixture::new(tag);
    f.build("pool").unwrap();
    let bytes = f.bytes();
    f.repo.write("data/al/allowlist.json", &bytes);
    write_key(&f.repo, "data/keys/k.bin", &KEY_A);
    fs::create_dir_all(f.repo.root.join("data/r2")).unwrap();
    let sums = fs::read(f.repo.root.join("data/res/SHA256SUMS")).unwrap();
    let sums = FileRef { path: "data/res4/SHA256SUMS".into(), sha256: f.repo.write("data/res4/SHA256SUMS", &sums) };
    f.repo.write(BASE_LOG, &chained(&base_log(&KEY_A), false));
    let src = Sources {
        pool_dir: "data/out/pool".into(),
        allowlist_v2: "data/al/allowlist.json".into(),
        key_file: "data/keys/k.bin".into(),
        key_commitment: sha256_hex(&KEY_A),
        protocol_sums: sums,
    };
    let p = CarveParams { targets: [("forum", 2, 1), ("wiki", 2, 1)].iter().map(|(d, n, g)| (d.to_string(), Target { lower_documents: *n, min_groups: *g })).collect(),
        b_res, mock: mock(10), horizon: 3, ar8_min_days: 1, iterations: 2, stage3_raised_domain: raised.into() };
    Pool { f, src, p }
}

fn pool(tag: &str) -> Pool {
    pool_with(tag, 1, "forum")
}

impl Pool {
    fn root(&self) -> &std::path::Path {
        &self.f.repo.root
    }

    fn draft(&self, out: &str) -> (Value, String) {
        carve_v3::draft(self.root(), &self.src, &self.p, BASE_LOG, &self.root().join(out)).unwrap()
    }

    /// A chained log holding the base entries and `extra`.
    fn log(&self, rel: &str, extra: &[Value]) {
        self.f.repo.write(rel, &chained(&[base_log(&KEY_A), extra.to_vec()].concat(), false));
    }

    fn apply(&self, draft: &str, sha: &str, log: &str, tag: &str) -> anyhow::Result<Value> {
        let r = self.root();
        let (al, po, ro) = (r.join(format!("data/r2/{tag}-allowlist-v3.json")), r.join(format!("data/out/{tag}")), r.join(format!("data/r2/{tag}-CARVE-A2.json")));
        carve_v3::apply(r, &ApplyArgs { draft, draft_sha256: sha, freeze_log: log, allowlist_out: &al, pool_out: &po, pool_id: None, record_out: &ro }, &self.p)
    }

    /// Draft, log and apply as `wa8`; returns the applied record.
    fn applied(&self) -> Value {
        let (_, sha) = self.draft("data/r2/a8");
        self.log("data/r2/FROZEN-A2.json", &confirmed(&sha));
        self.apply(DRAFT, &sha, "data/r2/FROZEN-A2.json", "wa8").unwrap()
    }

    fn lines(&self, rel: &str) -> Vec<Value> {
        fs::read_to_string(self.root().join(rel)).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn keys(&self, rows: &[&Value], rel: &str) -> FileRef {
        let rows: Vec<Value> = rows.iter().map(|g| json!({"group_key": g["group_key"]})).collect();
        self.f.repo.jsonl(rel, &rows)
    }
}

#[test]
fn the_draft_is_written_privately_with_both_group_sets_and_nothing_applied() {
    let pl = pool("carve-draft");
    let (d, sha) = pl.draft("data/r2/a8");
    let root = pl.root();
    assert_eq!(d["status"], "computed_draft");
    assert_eq!(sha, sha256_hex(&fs::read(root.join(DRAFT)).unwrap()));
    let r = &d["result"];
    assert!(r["verdict"]["pass"] == true, "{}", r["verdict"]);
    // Both group sets by digest, and set (i) checked by group id.
    assert_eq!(r["set_i_absent"], true);
    let si = &r["inputs"]["set_i"];
    assert!(si["consumed_test_assignment"]["groups"].as_u64().unwrap() >= si["groups"].as_u64().unwrap());
    assert_eq!(si["consumed_test_assignment"]["group_ids_sha256"].as_str().unwrap().len(), 64);
    let sii = &r["inputs"]["set_ii"];
    assert_eq!(sii["groups"], 102 + 22, "every stock group, kept or gate-excluded");
    assert_eq!(sii["index_remainder"][0]["domain"], "wiki", "the Wikipedia index remainder is part of set (ii)");
    // The protocol split set is always checked.
    assert_eq!(r["inputs"]["protocol_split_sums"]["matched"].as_array().unwrap().len(), 2);
    // The key's commitment is A6's, as logged.
    assert_eq!(r["key"]["commitment_sha256"], sha256_hex(&KEY_A));
    assert_eq!(r["key"]["a6_log_entry"]["utc"], "2026-11-01T01:17:11Z");
    assert_eq!(d["freeze_log_at_draft"]["path"], BASE_LOG);
    // Owner-only files, no key and no rank.
    let dir = root.join("data/r2/a8");
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    for f in [carve_v3::DRAFT_FILE, carve_v3::GROUPS_FILE] {
        let p = dir.join(f);
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
        let s = fs::read_to_string(&p).unwrap();
        assert!(!s.contains(&hex::encode(KEY_A)) && !s.contains("\"rank\""), "{f}");
    }
    let groups = pl.lines(&format!("data/r2/a8/{}", carve_v3::GROUPS_FILE));
    assert_eq!(groups.len() as u64, d["files"]["carve_groups"]["rows"].as_u64().unwrap());
    assert!(groups.iter().all(|g| g["domain"] == "forum" || g["domain"] == "wiki"));
    // Pool v2 is untouched.
    assert!(!pl.f.read_out("pool", "manifest.json").contains("carve_exclusion"));
    // The same inputs give the same draft; an existing directory is never replaced.
    let (d2, _) = pl.draft("data/r2/a8-again");
    assert_eq!(d2["result"], d["result"]);
    assert!(carve_v3::draft(root, &pl.src, &pl.p, BASE_LOG, &root.join("data/r2/a8")).is_err());
}

#[test]
fn the_draft_needs_the_commitment_a6_logged() {
    let mut pl = pool("carve-a6");
    // A6 logged another key's commitment: the key file and the argument agree, but A6 does not.
    pl.f.repo.write("data/r2/log-other.json", &chained(&base_log(&KEY_B), false));
    let e = format!("{:#}", carve_v3::draft(pl.root(), &pl.src, &pl.p, "data/r2/log-other.json", &pl.root().join("data/r2/x")).unwrap_err());
    assert!(e.contains("not the one A6 logged"), "{e}");
    assert!(!pl.root().join("data/r2/x").exists());
    // A key committed by A6 but given with another commitment is refused too.
    pl.src.key_commitment = sha256_hex(&KEY_B);
    let e = format!("{:#}", carve_v3::draft(pl.root(), &pl.src, &pl.p, "data/r2/log-other.json", &pl.root().join("data/r2/y")).unwrap_err());
    assert!(e.contains("does not match its A6 commitment"), "{e}");
    // A log without the chain is refused before anything is computed.
    pl.f.repo.write("data/r2/log-flat.json", &serde_json::to_vec(&json!({"log": base_log(&KEY_A)})).unwrap());
    let e = format!("{:#}", carve_v3::draft(pl.root(), &pl.src, &pl.p, "data/r2/log-flat.json", &pl.root().join("data/r2/z")).unwrap_err());
    assert!(e.contains("not hash-chained"), "{e}");
}

#[test]
fn the_protocol_split_set_must_list_the_same_files() {
    let mut pl = pool("carve-sums");
    let sums = fs::read(pl.root().join("data/res/SHA256SUMS")).unwrap();
    // A split set recording another digest for a consumed Test file refuses.
    let other = String::from_utf8(sums).unwrap().lines().map(|l| {
        if l.ends_with("./rules/test/groups.jsonl") { format!("{}  ./rules/test/groups.jsonl\n", sha256_hex(b"x")) } else { format!("{l}\n") }
    }).collect::<String>();
    let sha = pl.f.repo.write("data/res5/SHA256SUMS", other.as_bytes());
    pl.src.protocol_sums = FileRef { path: "data/res5/SHA256SUMS".into(), sha256: sha };
    let e = format!("{:#}", carve_v3::draft(pl.root(), &pl.src, &pl.p, BASE_LOG, &pl.root().join("data/r2/b8")).unwrap_err());
    assert!(e.contains("records another digest for rules/test/groups.jsonl"), "{e}");
}

#[test]
fn the_consumed_test_set_comes_from_the_test_files_on_the_load_path() {
    let pl = pool("carve-set-i");
    let carved: BTreeSet<String> = pl.p.targets.keys().cloned().collect();
    let (mut inp, rec) = carve_v3::load(pl.root(), &pl.src, Some(&KEY_A), &carved).unwrap();
    let mut want: Vec<String> = pl.lines("data/res/rules/test/groups.jsonl").iter().map(|r| r["group_key"].as_str().unwrap().to_string()).collect();
    assert_eq!(inp.consumed_test, want.iter().cloned().collect::<BTreeSet<_>>());
    let (n, d) = carve_v3::sorted_digest(&mut want);
    let ct = &rec["set_i"]["consumed_test_assignment"];
    assert_eq!((ct["groups"].as_u64(), ct["group_ids_sha256"].as_str()), (Some(n), Some(d.as_str())));
    assert!(rec["set_i"]["groups"].as_u64().unwrap() < n, "set (i) is the unreached part");
    // A carved stock group that is also a consumed-Test group is refused by set (i).
    let stray = inp.consumed_test.iter().find(|k| k.starts_with("forum:")).unwrap().clone();
    let forum = inp.domains.get_mut("forum").unwrap();
    let c0 = forum.candidates[0].clone();
    forum.candidates.insert(0, Candidate { group_key: stray, udocs: 0, ..c0 });
    let e = format!("{:#}", carve_v3::compute(&inp, &pl.p).err().expect("refused"));
    assert!(e.contains("set (i)"), "{e}");
    // Such a stock never reaches the carve: the reservation gate refuses it first.
    let mut f = Fixture::new("carve-set-i-gate");
    let path = f.allow.test_rules.groups[0].path.clone();
    let mut body = fs::read_to_string(f.repo.root.join(&path)).unwrap();
    body.push_str(&format!("{}\n", json!({"schema": stock::GROUP_SCHEMA, "split": "test", "domain": "forum", "group_key": "forum_author:3",
        "units": 2, "expected_docs_capped": 1.6, "max_docs_per_group": 2.0, "order_in_split_domain": 10,
        "contributor_key_sha256": [sha256_hex(b"forum_author:3")]})));
    f.allow.test_rules.groups[0].sha256 = f.repo.write(&path, body.as_bytes());
    let e = format!("{:#}", f.build("gate").unwrap_err());
    assert!(e.contains("refused by the reservation gate"), "{e}");
}

#[test]
fn apply_needs_the_user_decisions_and_rebuilds_pool_v2_at_w_a8() {
    let pl = pool("carve-apply");
    let (d, sha) = pl.draft("data/r2/a8");
    let w_a8 = d["result"]["w_a8"]["weights_ppm"].clone();
    let refused = |log: &[Value], tag: &str| {
        let rel = format!("data/r2/log-{tag}.json");
        pl.log(&rel, log);
        let e = format!("{:#}", pl.apply(DRAFT, &sha, &rel, tag).unwrap_err());
        assert!(!pl.root().join(format!("data/out/{tag}")).exists() && !pl.root().join(format!("data/r2/{tag}-allowlist-v3.json")).exists(), "{tag}");
        e
    };
    // No decisions; U1 alone (item 1.5 needs U1, U2, U16 and U17); U1 overridden.
    assert!(refused(&[drafted(&sha)], "a").contains("no user decision on U1"));
    assert!(refused(&[drafted(&sha), decisions(&[("U1", "confirmed")])], "b").contains("no user decision on U2"));
    let over = decisions(&[("U1", "overridden"), ("U2", "confirmed"), ("U16", "confirmed"), ("U17", "overridden")]);
    assert!(refused(&[drafted(&sha), over], "c").contains("U1 is recorded as overridden"));
    // The draft's digest must be logged in its field, not merely mentioned.
    let mention = json!({"utc": "2026-11-01T02:00:00Z", "event": "noted", "draft": sha});
    assert!(refused(&[mention, all_confirmed()], "d").contains("carve_draft_sha256"));
    // Another draft digest is refused before anything is written.
    pl.log("data/r2/log-e.json", &confirmed(&sha));
    assert!(pl.apply(DRAFT, &sha256_hex(b"x"), "data/r2/log-e.json", "e").is_err());
    // A tampered log is refused.
    let mut b = String::from_utf8(chained(&[base_log(&KEY_A), confirmed(&sha)].concat(), false)).unwrap();
    b = b.replacen("amendment 1", "amendment one", 1);
    pl.f.repo.write("data/r2/log-t.json", b.as_bytes());
    assert!(format!("{:#}", pl.apply(DRAFT, &sha, "data/r2/log-t.json", "t").unwrap_err()).contains("does not chain"));
    // Confirmed: applied.
    pl.log("data/r2/FROZEN-A2.json", &confirmed(&sha));
    let rec = pl.apply(DRAFT, &sha, "data/r2/FROZEN-A2.json", "wa8").unwrap();
    assert_eq!(rec["status"], "applied");
    assert_eq!(rec["result"], d["result"], "the applied record carries the draft's result");
    assert_eq!(rec["application"]["user_decisions"]["U1"]["decision"], "confirmed");
    assert_eq!(rec["application"]["user_decisions"].as_object().unwrap().len(), 4);
    assert_eq!(rec["application"]["freeze_log"]["chain"], "verified");
    let m = pl.f.out_json("wa8", "manifest.json");
    assert_eq!(m["domain_weights"]["weights_ppm"], w_a8);
    assert_eq!(m["allowlist"]["sha256"], rec["application"]["allowlist_v3"]["sha256"]);
    // The carve leaves the stock, with no group line and no receipt.
    let carve: BTreeSet<String> = pl.lines(&format!("data/r2/a8/{}", carve_v3::GROUPS_FILE)).iter().map(|g| g["group_key"].as_str().unwrap().to_string()).collect();
    let kept = pl.f.read_out("wa8", "groups.jsonl");
    let ex = pl.f.read_out("wa8", "exclusions.jsonl");
    for k in &carve {
        let q = format!("\"{k}\"");
        assert!(!kept.contains(&q) && !ex.contains(&q), "{k}");
    }
    assert_eq!(m["groups"]["rows"].as_u64().unwrap() + carve.len() as u64, 102 + 22);
    let by = &m["carve_exclusion"]["by_domain_role"];
    let total: u64 = by.as_object().unwrap().values().flat_map(|r| r.as_object().unwrap().values()).map(|v| v.as_u64().unwrap()).sum();
    assert_eq!(total, carve.len() as u64);
    assert!(pl.f.out_json("wa8", "gate.json")["carve_exclusion"].is_object());
    // Allow-list v3 is in the pool directory, and the rebuild used the original plan.
    let v3 = StockAllowlist::parse(pl.f.read_out("wa8", "allowlist-v3.json").as_bytes()).unwrap();
    assert_eq!(v3.schema, stock::STOCK_ALLOWLIST_SCHEMA_V3);
    assert_eq!(json!(v3.carve_exclusion.as_ref().unwrap().weights_ppm), w_a8);
    assert_eq!(pl.f.out_json("wa8", "capacity.json")["plan"], pl.f.out_json("pool", "capacity.json")["plan"]);
    // A failing draft is never applied.
    let mut bad = pl.p.clone();
    bad.targets.get_mut("wiki").unwrap().lower_documents = 10;
    let (fd, fsha) = carve_v3::draft(pl.root(), &pl.src, &bad, BASE_LOG, &pl.root().join("data/r2/fail")).unwrap();
    assert_eq!(fd["result"]["verdict"]["pass"], false);
    assert!(fd["result"]["failure"]["binding_domains"].is_object());
    pl.log("data/r2/log-f.json", &confirmed(&fsha));
    let e = format!("{:#}", pl.apply("data/r2/fail/CARVE-A2.draft.json", &fsha, "data/r2/log-f.json", "f").unwrap_err());
    assert!(e.contains("not a pass"), "{e}");
}

#[test]
fn apply_checks_every_output_path_before_writing() {
    let pl = pool("carve-outputs");
    let (_, sha) = pl.draft("data/r2/a8");
    pl.log("data/r2/FROZEN-A2.json", &confirmed(&sha));
    let r = pl.root();
    let try_apply = |al: &std::path::Path, po: &std::path::Path, ro: &std::path::Path| {
        carve_v3::apply(r, &ApplyArgs { draft: DRAFT, draft_sha256: &sha, freeze_log: "data/r2/FROZEN-A2.json", allowlist_out: al, pool_out: po, pool_id: None, record_out: ro }, &pl.p)
    };
    let po = r.join("data/out/wa8");
    // The allow-list's directory is missing, or the record is outside data/: refused, and no pool is built.
    let e = format!("{:#}", try_apply(&r.join("data/missing/al.json"), &po, &r.join("data/r2/CARVE-A2.json")).unwrap_err());
    assert!(e.contains("must exist"), "{e}");
    assert!(!po.exists());
    let e = format!("{:#}", try_apply(&r.join("data/r2/al.json"), &po, &r.join("CARVE-A2.json")).unwrap_err());
    assert!(e.contains("only under data/"), "{e}");
    assert!(!po.exists());
    let e = format!("{:#}", try_apply(&r.join("data/r2/al.json"), &po, &r.join("data/r2/al.json")).unwrap_err());
    assert!(e.contains("must differ"), "{e}");
    assert!(!po.exists() && !r.join("data/r2/al.json").exists());
    // The retry with good paths applies.
    let rec = try_apply(&r.join("data/r2/al.json"), &po, &r.join("data/r2/CARVE-A2.json")).unwrap();
    assert_eq!(rec["status"], "applied");
    assert!(po.join("manifest.json").exists() && r.join("data/r2/al.json").exists() && r.join("data/r2/CARVE-A2.json").exists());
}

#[test]
fn a_pool_build_refuses_a_carve_record_that_does_not_reproduce() {
    let pl = pool("carve-mismatch");
    pl.applied();
    let mut a = StockAllowlist::parse(pl.f.read_out("wa8", "allowlist-v3.json").as_bytes()).unwrap();
    // Other weights in the allow-list than in its record: refused before any write.
    let c = a.carve_exclusion.as_mut().unwrap();
    let k = c.weights_ppm.keys().find(|k| c.weights_ppm[*k] > 0).unwrap().clone();
    let j = c.weights_ppm.keys().find(|j| **j != k).unwrap().clone();
    *c.weights_ppm.get_mut(&k).unwrap() -= 1;
    *c.weights_ppm.get_mut(&j).unwrap() += 1;
    let out = pl.root().join("data/out/x");
    let e = format!("{:#}", pool_v2::build(pl.root(), &serde_json::to_vec(&a).unwrap(), &params(), &out).unwrap_err());
    assert!(e.contains("not the allow-list's"), "{e}");
    assert!(!out.exists());
    // v2 refuses a carve exclusion; v3 refuses to go without one.
    let mut v2 = a.clone();
    v2.schema = stock::STOCK_ALLOWLIST_SCHEMA.into();
    assert!(format!("{:#}", StockAllowlist::parse(&serde_json::to_vec(&v2).unwrap()).unwrap_err()).contains("needs allow-list v3"));
    let mut v3 = a.clone();
    v3.carve_exclusion = None;
    assert!(format!("{:#}", StockAllowlist::parse(&serde_json::to_vec(&v3).unwrap()).unwrap_err()).contains("needs its carve exclusion"));
}

/// Adds the active reference's lineage (a trainer bundle row keyed by `group`).
fn with_active_reference(f: &Fixture, a: &mut StockAllowlist, group: &str, tag: &str) {
    let bundle = f.repo.jsonl(&format!("data/rc2/{tag}/rows.jsonl"), &[json!({"id": "r2-1", "source_group": group, "text": "A synthetic trained passage.", "text_sha256": sha256_hex(b"r2")})]);
    let titles = FileRef { path: format!("data/rc2/{tag}/titles.txt"), sha256: f.repo.write(&format!("data/rc2/{tag}/titles.txt"), format!("{}\n", title("Nothing reserved")).as_bytes()) };
    let screen = json!({"schema": stock::LINEAGE_SCREEN_SCHEMA, "date": "2026-11-10",
        "inputs": {"raid_train_title_sha256": {"path": titles.path, "sha256": titles.sha256, "entries": 1}, "raid_train_rows": {"rows_sha256": bundle.sha256, "rows": 1}},
        "rules": [{"id": "S1_title", "rule": "skip by title hash"}, {"id": "S2_reddit_author", "rule": "compare author posts"}, {"id": "S3_text", "rule": "compare excerpt text"}]});
    let sp = format!("data/rc2/{tag}/screen.json");
    let ssha = f.repo.write(&sp, &serde_json::to_vec(&screen).unwrap());
    a.active_reference = Some(ActiveReference { reference: "r0-prime-synthetic".into(), trainer_bundle: "synthetic-bundle".into(), screen: FileRef { path: sp, sha256: ssha } });
    a.gate.push(gref(stock::ACTIVE_LINEAGE_ROLE.0, "trainer_bundle_rows", &bundle, 1));
    a.gate.push(gref(stock::ACTIVE_LINEAGE_ROLE.0, "title_sha256_list", &titles, 1));
}

/// A kept group of a domain the re-waterfill did (or did not) reduce.
fn kept_group(pl: &Pool, d: &Value, reduced_domain: bool) -> Option<String> {
    let reduced: BTreeSet<String> = serde_json::from_value(d["result"]["w_a8"]["reduced"].clone()).unwrap();
    let w: BTreeMap<String, u64> = serde_json::from_value(d["result"]["w_a8"]["weights_ppm"].clone()).unwrap();
    pl.lines("data/out/wa8/groups.jsonl").iter().find(|g| {
        let dm = g["domain"].as_str().unwrap();
        w[dm] > 0 && reduced.contains(dm) == reduced_domain
    }).map(|g| g["group_key"].as_str().unwrap().to_string())
}

#[test]
fn the_lineage_gate_re_runs_with_the_active_references_trainer_bundle() {
    let pl = pool("carve-lineage");
    let rec = pl.applied();
    let base = StockAllowlist::parse(pl.f.read_out("wa8", "allowlist-v3.json").as_bytes()).unwrap();
    let w: BTreeMap<String, u64> = serde_json::from_value(rec["result"]["w_a8"]["weights_ppm"].clone()).unwrap();
    // A group of a domain the re-waterfill did not reduce: excluded, weights unchanged.
    let free = kept_group(&pl, &rec, false).expect("a non-reduced domain");
    let mut a = base.clone();
    with_active_reference(&pl.f, &mut a, &free, "free");
    let m = pool_v2::build(pl.root(), &serde_json::to_vec(&a).unwrap(), &params(), &pl.root().join("data/out/lineage")).unwrap();
    let r = pl.f.receipts("lineage");
    assert_eq!(r[&free], ["reference_training_lineage:key"]);
    assert_eq!(m["domain_weights"]["weights_ppm"], json!(w));
    let gate = pl.f.out_json("lineage", "gate.json");
    assert_eq!(gate["active_reference_lineage"]["reference"], "r0-prime-synthetic");
    assert_eq!(gate["lineage_screen"]["path"], base.lineage_screen.path, "R0's lineage stays gated");
    // A reduced domain's group whose removal lowers its stock would change a manifest
    // weight: the build waits for an amendment.
    let reduced: BTreeSet<String> = serde_json::from_value(rec["result"]["w_a8"]["reduced"].clone()).unwrap();
    assert!(kept_group(&pl, &rec, true).is_some() && reduced.contains("wiki"), "{reduced:?}");
    let held = pl.lines("data/out/wa8/groups.jsonl").iter().find(|g| g["domain"] == "wiki").map(|g| g["group_key"].as_str().unwrap().to_string()).unwrap();
    let mut a = base.clone();
    with_active_reference(&pl.f, &mut a, &held, "held");
    let out = pl.root().join("data/out/lineage-held");
    let e = format!("{:#}", pool_v2::build(pl.root(), &serde_json::to_vec(&a).unwrap(), &params(), &out).unwrap_err());
    assert!(e.contains("item 1.11"), "{e}");
    assert!(!out.exists());
    // A hit on a carve group means Train-2 lineage touches Test-2 T or the reserve: its own refusal.
    let carve = pl.lines(&format!("data/r2/a8/{}", carve_v3::GROUPS_FILE));
    let mut a = base.clone();
    with_active_reference(&pl.f, &mut a, carve[0]["group_key"].as_str().unwrap(), "carve");
    let out = pl.root().join("data/out/lineage-carve");
    let e = format!("{:#}", pool_v2::build(pl.root(), &serde_json::to_vec(&a).unwrap(), &params(), &out).unwrap_err());
    assert!(e.contains("active reference's training lineage hits a carve group") && e.contains("Test-2 T or the mock reserve"), "{e}");
    assert!(!out.exists());
    // The role needs the active reference, and the reference its role and its own screen.
    let mut a = base.clone();
    with_active_reference(&pl.f, &mut a, &free, "norole");
    a.gate.retain(|g| g.role != stock::ACTIVE_LINEAGE_ROLE.0);
    assert!(format!("{:#}", StockAllowlist::parse(&serde_json::to_vec(&a).unwrap()).unwrap_err()).contains("needs at least one reference_training_lineage"));
    let mut a = base.clone();
    with_active_reference(&pl.f, &mut a, &free, "noref");
    a.active_reference = None;
    assert!(format!("{:#}", StockAllowlist::parse(&serde_json::to_vec(&a).unwrap()).unwrap_err()).contains("unknown gate role"));
    let mut a = base.clone();
    with_active_reference(&pl.f, &mut a, &free, "rebind");
    let other = pl.f.repo.jsonl("data/rc2/rebind/other.jsonl", &[json!({"id": "x", "source_group": "none"})]);
    let i = a.gate.iter().position(|g| g.role == stock::ACTIVE_LINEAGE_ROLE.0 && g.kind == "trainer_bundle_rows").unwrap();
    a.gate[i] = gref(stock::ACTIVE_LINEAGE_ROLE.0, "trainer_bundle_rows", &other, 1);
    let e = format!("{:#}", pool_v2::build(pl.root(), &serde_json::to_vec(&a).unwrap(), &params(), &pl.root().join("data/out/rebind")).unwrap_err());
    assert!(e.contains("the trainer bundle is not the screen's input"), "{e}");
}

// ------------------------------------------------------------------ release

/// The applied carve's groups split by role, and the release's fixed inputs.
struct Applied {
    rec: Value,
    rrel: String,
    rsha: String,
    allowlist: FileRef,
    groups: Vec<Value>,
}

impl Applied {
    fn of(pl: &Pool) -> Self {
        let rec = pl.applied();
        let rrel = rec["written"]["path"].as_str().unwrap().to_string();
        let rsha = rec["written"]["sha256"].as_str().unwrap().to_string();
        let allowlist = FileRef { path: rec["application"]["allowlist_v3"]["path"].as_str().unwrap().into(), sha256: rec["application"]["allowlist_v3"]["sha256"].as_str().unwrap().into() };
        let groups = pl.lines(&format!("data/r2/a8/{}", carve_v3::GROUPS_FILE));
        Applied { rec, rrel, rsha, allowlist, groups }
    }

    fn role(&self, role: &str) -> Vec<&Value> {
        self.groups.iter().filter(|g| g["role"] == role).collect()
    }
}

/// The freeze log after the seal and the Test-2 mock, with a release amendment.
fn built_log(sha: &str, sealed: &FileRef, used: &FileRef, release: Option<(&str, &str)>) -> Vec<Value> {
    let mut log = confirmed(sha);
    log.push(json!({"utc": "2026-11-12T00:00:00Z", "event": "seal", "SEAL-A2.json": sha256_hex(b"seal"), carve_v3::LOG_TEST2_T_FIELD: sealed.sha256}));
    log.push(json!({"utc": "2026-11-13T00:00:00Z", "event": "Test-2 mock built", "test2_mock_journal_sha256": sha256_hex(b"journal"), carve_v3::LOG_MOCK_USED_FIELD: used.sha256}));
    if let Some((rsha, cond)) = release {
        log.push(json!({"utc": "2026-11-20T00:00:00Z", "event": "amendment: release unused reserve", "releases_carve": rsha, "release_condition": cond}));
    }
    log
}

#[allow(clippy::too_many_arguments)]
fn release(pl: &Pool, record: (&str, &str), allowlist: &FileRef, pool_dir: &str, t2: &FileRef, mu: &FileRef, log: &str, out: &str) -> anyhow::Result<Value> {
    let r = pl.root();
    let (o, po) = (r.join(format!("data/r2/{out}")), r.join(format!("data/out/{out}")));
    carve_v3::release(r, &ReleaseArgs { record: record.0, record_sha256: record.1, allowlist, pool_dir, test2_t: t2, mock_used: mu, freeze_log: log, out: &o, pool_out: &po, pool_id: None }, &pl.p)
}

#[test]
fn unused_reserve_groups_return_only_by_a_logged_amendment() {
    let pl = pool("carve-release");
    let ap = Applied::of(&pl);
    let draft_sha = ap.rec["application"]["draft"]["sha256"].as_str().unwrap().to_string();
    let (test2, reserve) = (ap.role(carve_v3::ROLE_TEST2), ap.role(carve_v3::ROLE_RESERVE));
    assert!(reserve.len() >= 2 && !test2.is_empty(), "the synthetic carve needs a reserve to release");
    // The seal holds every Test-2 T group; the mock used the first Test-2 T group and the first reserve group.
    let sealed = pl.keys(&test2, "data/r2/test2-t.jsonl");
    let used = pl.keys(&[test2[0], reserve[0]], "data/r2/mock-used.jsonl");
    let rec = (ap.rrel.as_str(), ap.rsha.as_str());
    let go = |log: &str, record: (&str, &str), t2: &FileRef, mu: &FileRef, out: &str| release(&pl, record, &ap.allowlist, "data/out/wa8", t2, mu, log, out);
    // Without the amendment, under another condition, or from a draft: refused.
    pl.log("data/r2/log-none.json", &built_log(&draft_sha, &sealed, &used, None));
    assert!(format!("{:#}", go("data/r2/log-none.json", rec, &sealed, &used, "rel-a").unwrap_err()).contains("no freeze-log amendment releases this carve"));
    pl.log("data/r2/log-cond.json", &built_log(&draft_sha, &sealed, &used, Some((&ap.rsha, "the mock is built"))));
    assert!(go("data/r2/log-cond.json", rec, &sealed, &used, "rel-b").is_err());
    assert!(format!("{:#}", go("data/r2/log-none.json", (DRAFT, &draft_sha), &sealed, &used, "rel-c").unwrap_err()).contains("only an applied carve"));
    pl.log("data/r2/log-rel.json", &built_log(&draft_sha, &sealed, &used, Some((&ap.rsha, "u17_failed"))));
    // The Test-2 T membership and the mock's groups must be the logged ones.
    let other = pl.f.repo.jsonl("data/r2/test2-t-other.jsonl", &[json!({"group_key": test2[0]["group_key"]})]);
    assert!(format!("{:#}", go("data/r2/log-rel.json", rec, &other, &used, "rel-d").unwrap_err()).contains("test2_t_groups_sha256"));
    let stray = pl.f.repo.jsonl("data/r2/mock-stray.jsonl", &[json!({"group_key": "forum_author:not-carved"})]);
    assert!(format!("{:#}", go("data/r2/log-rel.json", rec, &sealed, &stray, "rel-e").unwrap_err()).contains("test2_mock_used_groups_sha256"));
    pl.log("data/r2/log-stray.json", &built_log(&draft_sha, &sealed, &stray, Some((&ap.rsha, "u17_failed"))));
    assert!(format!("{:#}", go("data/r2/log-stray.json", rec, &sealed, &stray, "rel-f").unwrap_err()).contains("outside the carve"));
    // A sealed membership lacking a carved Test-2 T group is refused: Test-2 T groups never return.
    if test2.len() > 1 {
        let short = pl.keys(&test2[1..], "data/r2/test2-t-short.jsonl");
        pl.log("data/r2/log-short.json", &built_log(&draft_sha, &short, &used, Some((&ap.rsha, "u17_failed"))));
        assert!(format!("{:#}", go("data/r2/log-short.json", rec, &short, &used, "rel-g").unwrap_err()).contains("never return"));
    }
    for out in ["rel-a", "rel-b", "rel-c", "rel-d", "rel-e", "rel-f", "rel-g"] {
        assert!(!pl.root().join(format!("data/r2/{out}")).exists() && !pl.root().join(format!("data/out/{out}")).exists(), "{out}");
    }
    // Released.
    let r = go("data/r2/log-rel.json", rec, &sealed, &used, "rel").unwrap();
    let res = &r["record"]["result"];
    assert_eq!(res["released"]["rows"], reserve.len() - 1);
    assert_eq!(res["kept"]["rows"], ap.groups.len() - (reserve.len() - 1));
    assert_eq!(res["mock_evidence"], "stays at W_A8 (§5.3 step 5)");
    assert_eq!(res["amendment"]["release_condition"], "u17_failed");
    assert!(res["tv_from_w_a8_ppm"].is_u64());
    // Every Test-2 T group and the used reserve stay excluded, used by the mock or not.
    let kept: BTreeSet<String> = pl.lines("data/r2/rel/carve-groups.jsonl").iter().map(|g| g["group_key"].as_str().unwrap().to_string()).collect();
    for g in &ap.groups {
        let k = g["group_key"].as_str().unwrap();
        let stays = g["role"] == carve_v3::ROLE_TEST2 || k == reserve[0]["group_key"];
        assert_eq!(kept.contains(k), stays, "{k}");
    }
    // The successor pool is rebuilt at the release's weights, with the released groups back.
    let m = pl.f.out_json("rel", "manifest.json");
    assert_eq!(m["domain_weights"]["weights_ppm"], res["weights"]["weights_ppm"]);
    let back = pl.f.read_out("rel", "groups.jsonl");
    for g in reserve.iter().skip(1) {
        assert!(back.contains(&format!("\"{}\"", g["group_key"].as_str().unwrap())));
    }
    for k in &kept {
        assert!(!back.contains(&format!("\"{k}\"")), "{k}");
    }
    // When the membership and the mock hold every reserve group, nothing is released.
    let all: Vec<&Value> = ap.groups.iter().collect();
    let mu_all = pl.keys(&all, "data/r2/mock-all.jsonl");
    pl.log("data/r2/log-all.json", &built_log(&draft_sha, &sealed, &mu_all, Some((&ap.rsha, "u17_failed"))));
    assert!(format!("{:#}", go("data/r2/log-all.json", rec, &sealed, &mu_all, "rel-h").unwrap_err()).contains("nothing to release"));
    assert!(!pl.root().join("data/r2/rel-h").exists());
}

#[test]
fn a_reserve_group_raised_into_test2_t_at_stage_3_never_returns() {
    // The raised domain needs at least two reserve groups: one raised, one released.
    let probe = pool_with("carve-raise-probe", 2, "forum");
    let ap = Applied::of(&probe);
    let by_domain = |d: &str| ap.role(carve_v3::ROLE_RESERVE).into_iter().filter(|g| g["domain"] == d).count();
    let raised = if by_domain("forum") >= 2 { "forum" } else { "wiki" };
    assert!(by_domain(raised) >= 2, "forum {} wiki {}", by_domain("forum"), by_domain("wiki"));
    drop(ap);
    drop(probe);
    let pl = pool_with("carve-raise", 2, raised);
    let ap = Applied::of(&pl);
    let draft_sha = ap.rec["application"]["draft"]["sha256"].as_str().unwrap().to_string();
    let test2 = ap.role(carve_v3::ROLE_TEST2);
    let reserve = ap.role(carve_v3::ROLE_RESERVE);
    let up: Vec<&Value> = reserve.iter().copied().filter(|g| g["domain"] == raised).take(1).collect();
    // Stage 3 moved one reserve group of the raised domain into Test-2 T; the mock used
    // only one Test-2 T group (at small B_M it takes fewer humans than Test-2 T holds).
    let sealed = pl.keys(&[test2.clone(), up.clone()].concat(), "data/r2/test2-t.jsonl");
    let used = pl.keys(&test2[..1], "data/r2/mock-used.jsonl");
    pl.log("data/r2/log-rel.json", &built_log(&draft_sha, &sealed, &used, Some((&ap.rsha, "effective_cohort_closed"))));
    let rec = (ap.rrel.as_str(), ap.rsha.as_str());
    let r = release(&pl, rec, &ap.allowlist, "data/out/wa8", &sealed, &used, "data/r2/log-rel.json", "rel").unwrap();
    let res = &r["record"]["result"];
    assert_eq!(res["test2_t_sealed"]["promoted_from_reserve_by_domain"][raised], 1);
    let kept: BTreeSet<String> = pl.lines("data/r2/rel/carve-groups.jsonl").iter().map(|g| g["group_key"].as_str().unwrap().to_string()).collect();
    let raised_key = up[0]["group_key"].as_str().unwrap();
    assert!(kept.contains(raised_key), "the raised group stays excluded although the mock did not use it");
    assert!(!pl.f.read_out("rel", "groups.jsonl").contains(&format!("\"{raised_key}\"")));
    assert!(test2.iter().all(|g| kept.contains(g["group_key"].as_str().unwrap())));
    assert_eq!(res["released"]["rows"], reserve.len() - 1);
    // Only the Stage-3 raised domain's reserve may move into Test-2 T.
    let other_domain = if raised == "forum" { "wiki" } else { "forum" };
    if let Some(g) = reserve.iter().copied().find(|g| g["domain"] == other_domain) {
        let bad = pl.keys(&[test2.clone(), vec![g]].concat(), "data/r2/test2-t-bad.jsonl");
        pl.log("data/r2/log-bad.json", &built_log(&draft_sha, &bad, &used, Some((&ap.rsha, "effective_cohort_closed"))));
        let e = format!("{:#}", release(&pl, rec, &ap.allowlist, "data/out/wa8", &bad, &used, "data/r2/log-bad.json", "rel-bad").unwrap_err());
        assert!(e.contains("only") && e.contains("raised at Stage 3"), "{e}");
    }
}

#[test]
fn the_release_keeps_the_active_references_lineage_exclusion() {
    let pl = pool("carve-release-active");
    let ap = Applied::of(&pl);
    let draft_sha = ap.rec["application"]["draft"]["sha256"].as_str().unwrap().to_string();
    // R_0' is live: the pool in force was rebuilt with its lineage, which blocked a stock group.
    let free = kept_group(&pl, &ap.rec, false).expect("a non-reduced domain");
    let mut a = StockAllowlist::parse(pl.f.read_out("wa8", "allowlist-v3.json").as_bytes()).unwrap();
    with_active_reference(&pl.f, &mut a, &free, "active");
    let ab = serde_json::to_vec_pretty(&a).unwrap();
    let allowlist = FileRef { path: "data/r2/allowlist-v3-active.json".into(), sha256: pl.f.repo.write("data/r2/allowlist-v3-active.json", &ab) };
    pool_v2::build(pl.root(), &ab, &params(), &pl.root().join("data/out/active")).unwrap();
    let (test2, reserve) = (ap.role(carve_v3::ROLE_TEST2), ap.role(carve_v3::ROLE_RESERVE));
    let sealed = pl.keys(&test2, "data/r2/test2-t.jsonl");
    let used = pl.keys(&test2[..1], "data/r2/mock-used.jsonl");
    pl.log("data/r2/log-rel.json", &built_log(&draft_sha, &sealed, &used, Some((&ap.rsha, "effective_cohort_closed"))));
    let rec = (ap.rrel.as_str(), ap.rsha.as_str());
    // The release must start from the allow-list in force and the pool built from it.
    let e = format!("{:#}", release(&pl, rec, &allowlist, "data/out/wa8", &sealed, &used, "data/r2/log-rel.json", "rel-x").unwrap_err());
    assert!(e.contains("was not built from"), "{e}");
    let r = release(&pl, rec, &allowlist, "data/out/active", &sealed, &used, "data/r2/log-rel.json", "rel").unwrap();
    let res = &r["record"]["result"];
    assert_eq!(res["stock_in_force"]["active_reference"]["reference"], "r0-prime-synthetic");
    assert_eq!(res["released"]["rows"], reserve.len());
    // The successor allow-list keeps the active reference, and the lineage-blocked group stays out.
    let succ = StockAllowlist::parse(&fs::read(pl.root().join("data/r2/rel/allowlist-v3.json")).unwrap()).unwrap();
    assert_eq!(succ.active_reference, a.active_reference);
    assert_eq!(json!(succ.gate), json!(a.gate));
    let back = pl.f.read_out("rel", "groups.jsonl");
    assert!(!back.contains(&format!("\"{free}\"")), "the lineage-blocked group is back in the stock");
    assert_eq!(pl.f.receipts("rel")[&free], ["reference_training_lineage:key"]);
    for g in &reserve {
        assert!(back.contains(&format!("\"{}\"", g["group_key"].as_str().unwrap())));
    }
    // The re-waterfill was computed on the stock in force, so the rebuild reproduces it.
    assert_eq!(pl.f.out_json("rel", "manifest.json")["domain_weights"]["weights_ppm"], res["weights"]["weights_ppm"]);
}

#[test]
fn v2_builds_are_unchanged_by_the_carve_code() {
    let mut f = Fixture::new("carve-v2");
    let m = f.build("v2").unwrap();
    assert!(m.get("carve_exclusion").is_none());
    let gate = f.out_json("v2", "gate.json");
    assert!(gate.get("carve_exclusion").is_none() && gate.get("active_reference_lineage").is_none());
    let cap = f.out_json("v2", "capacity.json");
    assert!(cap["stock"]["by_domain"]["forum"].get("carved_groups").is_none());
    // A v2 allow-list cannot name an active reference.
    let mut a = f.allow.clone();
    with_active_reference(&f, &mut a, "forum_author:1", "v2");
    assert!(format!("{:#}", StockAllowlist::parse(&serde_json::to_vec(&a).unwrap()).unwrap_err()).contains("needs allow-list v3"));
}

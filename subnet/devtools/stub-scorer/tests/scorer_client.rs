//! The stub's load protocol, read by the scorer client the detect Worker's
//! lane uses (`slop_ninja_scorer_client`), step by step as the lane drives
//! it: health, spec, prepare, chunks, load, ready, score.

use serde_json::{Value, json};
use slop_ninja_artifact_check::spec::{PART_BYTES, Version};
use slop_ninja_dev_http::Call;
use slop_ninja_dev_stub_scorer::instance::Instance;
use slop_ninja_dev_stub_scorer::result::sha;
use slop_ninja_scorer_client::{self as sc, Entry, Ours, Prep, Slot};

fn manifest(sizes: &[u64]) -> Vec<u8> {
    let files: Vec<Value> = sizes
        .iter()
        .enumerate()
        .map(|(i, b)| json!({"name": format!("f{i}"), "role": "r", "bytes": b, "sha256": "00".repeat(32),
            "bucket": "admission", "key": format!("k/{i}"), "etag": "e"}))
        .collect();
    serde_json::to_vec(&json!({"schema": "s", "profile_id": "p", "contract": "c", "graph": "f0", "files": files})).unwrap()
}

fn healthz(i: &mut Instance, now: u64) -> Value {
    i.handle(&Call::new("GET", "/healthz"), now, false).body_json()
}

#[test]
fn the_lane_loads_and_scores_through_the_client() {
    let m = manifest(&[7, PART_BYTES + 1]);
    let pkg = sha(&m);
    let spec_digest = Version::V4.spec_sha256();
    let mut i = Instance::new(0);
    let h = healthz(&mut i, 1);
    // The lane's build checks: one session, two threads, the spec served.
    assert_eq!((h.pointer("/build/sessions").and_then(Value::as_u64), h.pointer("/build/threads").and_then(Value::as_u64)), (Some(1), Some(2)));
    let spec = sc::spec_sent(&h, Some(&spec_digest)).expect("spec served");
    assert_eq!(sc::slot(&h, &pkg, None, spec), Slot::Empty);
    assert_eq!(sc::entry(Some(&h), &pkg, None, spec), Entry::Keep);
    let r = i.handle(&Call::new("POST", &sc::prepare_path(&pkg, None, spec)).body(m.clone()), 2, false);
    let missing = sc::prepare_reply(r.status, &r.body_json()).expect("prepared");
    assert_eq!(missing, vec![(0, 0), (1, 0), (1, PART_BYTES)]);
    assert_eq!(sc::slot(&healthz(&mut i, 3), &pkg, None, spec), Slot::Ours(Ours::Receiving));
    for (index, offset) in missing {
        let len = [7, PART_BYTES + 1][index] - offset;
        let c = i.chunk(&sc::chunk_path(index, offset), len.min(PART_BYTES), 4);
        assert!(matches!(c.status, 200 | 201), "{}", c.status);
    }
    let l = i.handle(&Call::new("POST", &sc::load_path(&pkg, None, spec)), 5, false);
    assert_eq!(sc::load_reply(l.status, &l.body_json()), None);
    let h = healthz(&mut i, 6);
    assert_eq!(sc::slot(&h, &pkg, None, spec), Slot::Ours(Ours::Ready));
    let docs = vec![(1u32, format!("sn-stub:band=strong {}", "word ".repeat(60)), String::new())];
    let s = i.handle(&Call::new("POST", "/v1/score").body(sc::score_body(&pkg, None, &docs, true)), 7, false);
    assert_eq!(s.status, 200);
    let v = s.body_json();
    assert_eq!(v["package"], pkg);
    assert_eq!(v["results"][0]["id"], "1");
    assert_eq!(v["results"][0]["span_status"], "ok");
    // A probe of another package meets an instance bound elsewhere.
    assert_eq!(sc::entry(Some(&h), &"ab".repeat(32), None, spec), Entry::Fresh);
    let other = i.handle(&Call::new("POST", &sc::prepare_path(&"ab".repeat(32), None, spec)).body(m), 8, false);
    assert_eq!(sc::prepare_reply(other.status, &other.body_json()), Err(Prep::Infra(sc::INSTANCE_BOUND)));
}

#[test]
fn a_refused_load_is_definitive_to_the_client() {
    let m = manifest(&[3]);
    let pkg = sha(&m);
    let mut i = Instance::new(0);
    i.handle(&Call::new("POST", "/__stub/switch").json(&json!({"load_error": "refused graph_class: not a served class"})), 0, false);
    i.handle(&Call::new("POST", &sc::prepare_path(&pkg, None, None)).body(m), 1, false);
    i.chunk(&sc::chunk_path(0, 0), 3, 2);
    i.handle(&Call::new("POST", &sc::load_path(&pkg, None, None)), 3, false);
    let h = healthz(&mut i, 4);
    assert_eq!(sc::slot(&h, &pkg, None, None), Slot::Ours(Ours::Failed));
    assert_eq!(sc::entry(Some(&h), &pkg, None, None), Entry::Refused("refused graph_class"));
    // Any other failure is retried on a fresh instance.
    i.handle(&Call::new("POST", "/__stub/switch").json(&json!({"load_error": "ort: session"})), 5, false);
    i.handle(&Call::new("POST", "/__stub/destroy"), 5, false);
    let m = manifest(&[3]);
    i.handle(&Call::new("POST", &sc::prepare_path(&pkg, None, None)).body(m), 6, false);
    i.chunk(&sc::chunk_path(0, 0), 3, 6);
    i.handle(&Call::new("POST", &sc::load_path(&pkg, None, None)), 6, false);
    assert_eq!(sc::entry(Some(&healthz(&mut i, 7)), &pkg, None, None), Entry::Fresh);
}

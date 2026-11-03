//! End-to-end tests on synthetic inputs (no corpus text).

use crate::{archive, db, plan, runner, verify};
use serde_json::json;
use std::fs;
use std::path::Path;

fn write(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}

fn long(tag: &str) -> String {
    format!("{tag} {}", "The synthetic sentence is here for testing and it has enough words to pass. ".repeat(4))
}

fn plan_for(root: &Path, forbidden: &Path) -> plan::Plan {
    serde_json::from_value(json!({
        "schema": plan::PLAN_SCHEMA,
        "forbidden_prefixes": [forbidden.to_string_lossy()],
        "sources": [
            {"id": "a", "adapter": "generic", "priority": 1, "root": root.join("a").to_string_lossy(),
             "inputs": [{"pattern": "*/rows.jsonl", "role": "rows"}, {"pattern": "SHA256SUMS", "role": "archive", "format": "raw"}],
             "params": {"text": "text", "text_sha256": "text_sha256", "id": "id",
                        "fields": {"source_group": "source_group", "origin": "origin"},
                        "view": {"experiment": "e", "split_paths": ["split"], "kind": "bundle", "roles": ["rows"]}}},
            {"id": "b", "adapter": "generic", "priority": 2, "root": root.join("b").to_string_lossy(),
             "inputs": [{"pattern": "docs.jsonl", "role": "rows"}],
             "params": {"text": "text", "id": "id", "fields": {"source_group": "g"},
                        "view": {"experiment": "e", "split_paths": ["split"], "kind": "split_file"}}},
            {"id": "p", "adapter": "pangram", "priority": 3, "root": root.join("p").to_string_lossy(),
             "inputs": [{"pattern": "request.json", "role": "bulk_request"}, {"pattern": "page-*.json", "role": "observations"},
                        {"pattern": "page-*.json", "role": "observations"}],
             "params": {"store": "s"}}
        ]
    }))
    .unwrap()
}

#[test]
fn ingest_dedups_rebuilds_and_verifies() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("in");
    let store = t.path().join("store");
    let (x, y) = (long("x"), long("y"));
    let hx = hex::encode(crate::skeleton::sha256(x.as_bytes()));
    let rows = format!(
        "{}\n{}\n",
        json!({"id": "1", "split": "train", "text": x, "text_sha256": hx, "source_group": "g1", "origin": "human_only"}),
        json!({"id": "2", "split": "dev_iter", "text": y, "source_group": "g2", "origin": "model_only"})
    );
    write(&root.join("a/train/rows.jsonl"), &rows);
    let digest = hex::encode(crate::skeleton::sha256(rows.as_bytes()));
    write(&root.join("a/SHA256SUMS"), &format!("{digest}  train/rows.jsonl\n"));
    // The same text again in another source, and a cross-split group leak.
    write(&root.join("b/docs.jsonl"), &format!("{}\n", json!({"id": "3", "split": "cal", "text": x, "g": "g1"})));
    write(&root.join("p/request.json"), &json!({"model": "m", "items": [{"id": "1", "text": x}]}).to_string());
    write(&root.join("p/page-0.json"), &json!({"bulk_id": "B", "items": [{"id": "1", "index": 0, "result": {"fraction_ai": 0.5, "version": "4.1"}}]}).to_string());
    write(&root.join("p/page-1.json"), &json!({"bulk_id": "B", "offset": 1, "items": [{"id": "1", "index": 0, "result": {"fraction_ai": 0.5, "version": "4.1"}}]}).to_string());

    let p = plan_for(&root, &t.path().join("sealed"));
    fs::create_dir_all(&store).unwrap();
    let conn = db::open(&store.join("corpus.sqlite")).unwrap();
    runner::run(&conn, &store, &p, &[], false).unwrap();
    // Idempotent: a second run skips every source and adds nothing.
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM record", [], |r| r.get(0)).unwrap();
    runner::run(&conn, &store, &p, &[], false).unwrap();
    let after: i64 = conn.query_row("SELECT COUNT(*) FROM record", [], |r| r.get(0)).unwrap();
    assert_eq!(before, after);

    let texts: i64 = conn.query_row("SELECT COUNT(*) FROM text", [], |r| r.get(0)).unwrap();
    assert_eq!(texts, 2, "one row per unique text");
    let obs: i64 = conn.query_row("SELECT COUNT(*) FROM observation", [], |r| r.get(0)).unwrap();
    assert_eq!(obs, 1, "repeated result pages of one bulk job are one observation");

    let mut out = Vec::new();
    let got = archive::rebuild_file(&conn, &store.join("packs"), &digest, &mut out).unwrap();
    assert_eq!(got, digest);
    assert_eq!(out, rows.as_bytes());

    let rep = verify::run(&conn, &store, None, true).unwrap();
    assert_eq!(rep["views"]["ok"], json!(2));
    assert_eq!(rep["split_leaks"]["e"]["group_leaks"], json!(1));
    assert_eq!(rep["split_leaks"]["e"]["text_leaks"], json!(1));
}

#[test]
fn forbidden_inputs_are_refused() {
    let t = tempfile::tempdir().unwrap();
    let sealed = t.path().join("sealed");
    write(&sealed.join("x.jsonl"), "{}\n");
    let p: plan::Plan = serde_json::from_value(json!({
        "schema": plan::PLAN_SCHEMA, "forbidden_prefixes": [sealed.to_string_lossy()],
        "sources": [{"id": "s", "adapter": "archive", "priority": 1, "root": t.path().to_string_lossy(),
                     "inputs": [{"pattern": "**", "role": "archive"}]}]})).unwrap();
    let conn = db::open(&t.path().join("store.sqlite")).unwrap();
    let e = runner::run(&conn, &t.path().join("st"), &p, &[], false).unwrap_err();
    assert!(e.to_string().contains("forbidden"), "{e}");
}

#[test]
fn frontier_response_extraction() {
    use crate::adapters::frontier::response_of;
    assert_eq!(response_of(&json!({"type": "result", "result": "r"})).as_deref(), Some("r"));
    assert_eq!(response_of(&json!({"type": "item.completed", "item": {"type": "agent_message", "text": "c"}})).as_deref(), Some("c"));
    assert_eq!(response_of(&json!({"type": "item.completed", "item": {"type": "reasoning", "text": "no"}})), None);
}

#[test]
fn frontier_receipts_keep_the_logged_model_under_either_call_log_key() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("in");
    let rows = |model: &str| json!([{"status": "success", "model": model, "reasoning_effort": "medium"}]);
    let current = json!({"schema": "slop_ninja_frontier_attempt_receipt_v2", "status": "failed", "request_log": rows("m-new")});
    // An earlier receipt keeps its rows under an older key, found by the digest
    // of its name; this plan names a synthetic key's digest.
    let earlier = json!({"status": "failed", "older_log": rows("m-old")});
    let older = hex::encode(crate::skeleton::sha256(b"older_log"));
    write(&root.join("runs/l/r1/attempt-1/receipt.json"), &current.to_string());
    write(&root.join("runs/l/r2/attempt-1/receipt.json"), &earlier.to_string());
    let p: plan::Plan = serde_json::from_value(json!({
        "schema": plan::PLAN_SCHEMA,
        "sources": [{"id": "w", "adapter": "frontier_wave", "priority": 1, "root": root.to_string_lossy(),
                     "inputs": [{"pattern": "runs/*/*/attempt-*/receipt.json", "role": "receipt"}],
                     "params": {"wave": "w1", "earlier_call_log_key_sha256": older}}]})).unwrap();
    let store = t.path().join("store");
    fs::create_dir_all(&store).unwrap();
    let conn = db::open(&store.join("corpus.sqlite")).unwrap();
    runner::run(&conn, &store, &p, &[], false).unwrap();
    let got: Vec<(String, String)> = conn
        .prepare("SELECT request_id, params_json FROM event WHERE kind = 'frontier_attempt' ORDER BY request_id").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(got.len(), 2);
    assert!(got[0].0 == "r1" && got[0].1.contains("\"m-new\"") && !got[0].1.contains("m-old"), "{got:?}");
    assert!(got[1].0 == "r2" && got[1].1.contains("\"m-old\"") && !got[1].1.contains("m-new"), "{got:?}");
}

#[test]
fn journals_pair_intents_with_responses_and_keep_unanswered_intents() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("in");
    let doc = long("m");
    let body = json!({"model": "a-comp-x", "messages": [{"role": "user", "content": "Write it."}], "temperature": 0.8, "seed": 3});
    let raw = json!({"choices": [{"message": {"content": doc}, "finish_reason": "stop"}]}).to_string();
    let lines = [
        json!({"kind": "intent", "request_id": "r1", "item_id": "i1", "attempt": 0, "generator": "x", "split": "train", "t": 1, "body": body}),
        json!({"kind": "response", "request_id": "r1", "item_id": "i1", "attempt": 0, "generator": "x", "status": 200, "raw_body": raw,
               "verdict": {"accepted": true, "reasons": [], "text": doc}, "t_start": 1, "t_end": 2}),
        json!({"kind": "intent", "request_id": "r2", "item_id": "i2", "attempt": 0, "generator": "x", "split": "train", "t": 3, "body": body}),
    ];
    write(&root.join("plan/items.jsonl"), &format!("{}\n", json!({"item_id": "i1", "kind": "mirror", "parent_id": "p", "cohort": "c"})));
    write(&root.join("runs/x/journal-1-w0.jsonl"), &format!("{}\n{}\n{}\n", lines[0], lines[1], lines[2]));
    let p: plan::Plan = serde_json::from_value(json!({
        "schema": plan::PLAN_SCHEMA,
        "sources": [{"id": "j", "adapter": "local_journal", "priority": 1, "root": root.to_string_lossy(),
                     "inputs": [{"pattern": "plan/items.jsonl", "role": "plan_items"}, {"pattern": "runs/*/journal-*.jsonl", "role": "journal"}],
                     "params": {"family": "f"}}]})).unwrap();
    let store = t.path().join("store");
    fs::create_dir_all(&store).unwrap();
    let conn = db::open(&store.join("corpus.sqlite")).unwrap();
    runner::run(&conn, &store, &p, &[], false).unwrap();
    let st: Vec<(String, Option<String>)> = conn
        .prepare("SELECT status, output_text_sha256 FROM event ORDER BY request_id").unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(st.len(), 2);
    assert_eq!(st[0].0, "accepted");
    assert_eq!(st[0].1.as_deref(), Some(hex::encode(crate::skeleton::sha256(doc.as_bytes()))).as_deref());
    assert_eq!(st[1].0, "no_response");
    let prompt: Option<String> = conn.query_row("SELECT prompt_sha256 FROM event WHERE request_id = 'r1'", [], |r| r.get(0)).unwrap();
    assert_eq!(prompt.unwrap(), hex::encode(crate::skeleton::sha256(b"Write it.")));
    let outputs: i64 = conn.query_row("SELECT COUNT(*) FROM record WHERE role = 'local_output'", [], |r| r.get(0)).unwrap();
    assert_eq!(outputs, 1, "an accepted mirror output is a corpus text");
}

#[test]
fn export_writes_checksummed_tables() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("in");
    write(&t.path().join("datasets/snap/train.jsonl"), &format!("{}\n", json!({"id": "1", "split": "train", "text": long("pub")})));
    write(&root.join("private/docs.jsonl"), &format!("{}\n", json!({"id": "2", "split": "train", "text": long("priv")})));
    // A private root that merely contains `/datasets/` is not a published snapshot.
    write(&root.join("private/datasets/leak/train.jsonl"), &format!("{}\n", json!({"id": "3", "split": "train", "text": long("leak")})));
    let p: plan::Plan = serde_json::from_value(json!({
        "schema": plan::PLAN_SCHEMA,
        "sources": [
            {"id": "leak", "adapter": "generic", "priority": 3, "root": root.join("private/datasets/leak").to_string_lossy(),
             "inputs": [{"pattern": "train.jsonl", "role": "records"}],
             "params": {"text": "text", "id": "id", "view": {"experiment": "leak", "split_paths": ["split"]}}},
            {"id": "pub", "adapter": "generic", "priority": 1, "root": t.path().join("datasets/snap").to_string_lossy(),
             "inputs": [{"pattern": "train.jsonl", "role": "records"}],
             "params": {"text": "text", "id": "id", "view": {"experiment": "snap", "split_paths": ["split"]}}},
            {"id": "priv", "adapter": "generic", "priority": 2, "root": root.join("private").to_string_lossy(),
             "inputs": [{"pattern": "docs.jsonl", "role": "records"}], "params": {"text": "text", "id": "id"}}]})).unwrap();
    let store = t.path().join("store");
    fs::create_dir_all(&store).unwrap();
    let conn = db::open(&store.join("corpus.sqlite")).unwrap();
    runner::run(&conn, &store, &p, &[], false).unwrap();
    crate::link::run(&conn).unwrap();
    let all = crate::export::run(&conn, &store, &t.path().join("all"), "all").unwrap();
    assert_eq!(all["files"]["texts.jsonl.zst"]["rows"], json!(3));
    fs::create_dir_all(t.path().join(".git")).unwrap();
    let red = crate::export::run(&conn, &store, &t.path().join("red"), "redistributable").unwrap();
    let views = String::from_utf8(zstd::decode_all(fs::File::open(t.path().join("red/views.jsonl.zst")).unwrap()).unwrap()).unwrap();
    assert!(views.contains("\"path\":\"datasets/snap/train.jsonl\""), "{views}");
    assert!(!views.contains("leak"), "a private root containing /datasets/ was exported: {views}");
    assert!(!views.contains(&t.path().to_string_lossy().to_string()), "host path leaked: {views}");
    assert_eq!(red["files"]["texts.jsonl.zst"]["rows"], json!(1));
    assert!(red["files"].get("observations.jsonl.zst").is_none());
    let bytes = zstd::decode_all(fs::File::open(t.path().join("red/texts.jsonl.zst")).unwrap()).unwrap();
    assert!(String::from_utf8(bytes).unwrap().contains("pub The synthetic"));
}

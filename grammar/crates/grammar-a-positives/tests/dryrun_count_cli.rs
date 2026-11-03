//! `slop_ninja_a_positives dryrun-count` and `dryrun-merge` (B23) on
//! synthetic text: a plan's profile-off journals, written by `generate`
//! against a local stand-in server, are re-verdicted by `renorm` (the §4.3
//! dry run's path), and the counting tool reads only the re-verdict's
//! records, output journals and the batch's recorded inputs.
use grammar_a_positives::text;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;

const BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_a_positives");

const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
    "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
];

fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
    let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut out: Vec<String> = Vec::new();
    for _ in 0..n {
        let ws: Vec<&str> = (0..len)
            .map(|_| {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                vocab[((st >> 33) as usize) % vocab.len()]
            })
            .collect();
        let s = ws.join(" ");
        out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
    }
    out.join(" ")
}

/// Synthetic humans; each holds closed em dashes (the dash convention its
/// children take).
fn write_humans(path: &Path, split: &str) {
    let mut out = String::new();
    for (d, coll) in [("fw", "fineweb_cc_historical"), ("wp", "wikipedia_enwiki_historical")] {
        for k in 0..8u64 {
            let t = format!("{} The pier stayed open\u{2014}briefly. {}\n\n{}", prose(&SYN, 100 + k, 3, 11), prose(&SYN, 150 + k, 2, 11), prose(&SYN, 200 + k, 7, 11));
            let row = json!({"id":format!("{split}-{d}-{k}"),"frozen_split":split,"origin":"human_only","text":t,
                "text_sha256":text::sha_hex(t.as_bytes()),"word_count":text::words(&t),"collection":coll,
                "provenance":{"title":format!("synthetic {k}")},"source_group":format!("g-{d}-{k}"),"domain":"web"});
            out.push_str(&serde_json::to_string(&row).unwrap());
            out.push('\n');
        }
    }
    std::fs::write(path, out).unwrap();
}

fn read_http(s: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let len = head
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok()))
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = s.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Some((head.lines().next()?.to_owned(), buf[head_end..head_end + len].to_vec()))
}

const ALT: [&str; 20] = [
    "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
    "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
];

/// A llama-server stand-in on loopback for the profile-off `generate` run
/// that writes the journals: `/props`, `/v1/models`, and chat replies. A
/// call gets about 77 words (plan v3's continuation target): a list of seven
/// lines for odd calls, which the profile-off checks accept and profile v2's
/// form screen rejects, and prose with two spaced hyphens one space apart for
/// even ones (amendment 7's code panics on them under the parents' closed
/// em-dash convention; the landed code renders them).
fn serve() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        let mut n = 0u64;
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let Some((line, _)) = read_http(&mut s) else { continue };
            let out = if line.starts_with("QUIT") {
                break;
            } else if line.starts_with("GET /props") {
                json!({"model_path": "/models/m.gguf", "chat_template": "t"})
            } else if line.starts_with("GET /v1/models") {
                json!({"data": [{"id": "m"}]})
            } else {
                n += 1;
                let reply = if n % 2 == 1 {
                    (0..7).map(|k| format!("- {}", prose(&ALT, 300 + n * 10 + k, 1, 10))).collect::<Vec<_>>().join("\n")
                } else {
                    format!("{} The keeper wrote acorn - - bramble twice. {}", prose(&ALT, 400 + n, 3, 11), prose(&ALT, 700 + n, 3, 11))
                };
                json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}],"usage":{},"timings":{}})
            };
            let out = serde_json::to_vec(&out).unwrap();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&out).unwrap();
        }
    });
    (format!("http://{addr}"), h)
}

fn tokenizer_json() -> &'static str {
    r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
}

fn index(d: &Path) -> std::path::PathBuf {
    let index = d.join("index");
    std::fs::create_dir_all(&index).unwrap();
    let mut files = serde_json::Map::new();
    for k in ["exact", "normalized", "folded", "shingles_ws", "shingles_folded"] {
        std::fs::write(index.join(format!("{k}.u64")), b"").unwrap();
        files.insert(k.into(), json!({"sha256": text::sha_hex(b"")}));
    }
    std::fs::write(index.join("index.json"), serde_json::to_vec(&json!({"schema": "slop_ninja_a_pool_protected_index_v1", "key_files": files})).unwrap()).unwrap();
    index
}

/// Copies directory `from` to the new directory `to`.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let q = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &q);
        } else {
            std::fs::copy(&p, &q).unwrap();
        }
    }
}

fn append(p: &Path, b: &[u8]) {
    let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
    f.write_all(b).unwrap();
}

fn jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}


fn run(args: &[&str]) -> std::process::Output {
    std::process::Command::new(BIN).args(args).output().unwrap()
}

/// Lines of every split but Train that no reader may parse.
const NOT_TRAIN: &[u8] = b"{\"split\":\"cal\",\"item_id\":\"c\",\x00 never parsed\n{\"split\":\"dev_iter\", broken\n{\"split\":\"dev_select\",\"item_id\":\"x\",\x00 never parsed\n{\"id\":\"t\", \"split\" : \"test\", broken\n";

/// Synthetic R0 Train build rows for the plan's train items (unparsable
/// lines of every other split beside them).
fn r0_rows(pdir: &Path, out: &Path) {
    let mut b = Vec::new();
    for it in jsonl(&pdir.join("items.jsonl")) {
        if it["split"] != "train" {
            continue;
        }
        let origin = if it["kind"] == "mirror" { "model_only" } else { "mixed" };
        let row = json!({"id": it["item_id"], "split": "train", "source_kind": "local_positive", "origin": origin, "domain": it["domain"]});
        b.extend(serde_json::to_vec(&row).unwrap());
        b.push(b'\n');
    }
    b.extend_from_slice(NOT_TRAIN);
    std::fs::write(out, b).unwrap();
}

#[test]
fn dryrun_count_reads_a_local_batchs_records_and_merges() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (tr, di) = (d.join("train.jsonl"), d.join("dev-iter.jsonl"));
    write_humans(&tr, "train");
    write_humans(&di, "dev_iter");
    let pdir = d.join("plan");
    let o = run(&["plan-v3", "--train", tr.to_str().unwrap(), "--dev-iter", di.to_str().unwrap(), "--splits", "train,dev_iter", "--out", pdir.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let meta: Value = serde_json::from_slice(&std::fs::read(pdir.join("plan.json")).unwrap()).unwrap();
    let generator = meta["request_files"].as_object().unwrap().keys().next().unwrap().clone();
    let (endpoint, server) = serve();
    let cfg = d.join("cfg.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": generator, "allowed_splits": ["train", "dev_iter"], "endpoint": endpoint, "model_path": "/models/m.gguf", "alias": "m"})).unwrap()).unwrap();
    let idx = index(d);
    let tok = d.join("tok.json");
    std::fs::write(&tok, tokenizer_json()).unwrap();
    let runs = d.join("runs");
    let copy_t = d.join("plan-train");
    let o = run(&["split-copy", "--plan", pdir.to_str().unwrap(), "--split", "train", "--out", copy_t.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = run(&["generate", "--plan", copy_t.to_str().unwrap(), "--generator", generator.as_str(), "--config", cfg.to_str().unwrap(), "--runs", runs.to_str().unwrap(),
        "--index", idx.to_str().unwrap(), "--tokenizer", tok.to_str().unwrap(), "--workers", "1", "--splits", "train", "--limit", "400"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    TcpStream::connect(endpoint.trim_start_matches("http://")).unwrap().write_all(b"QUIT / HTTP/1.1\r\n\r\n").unwrap();
    server.join().unwrap();
    // R0's records: every train item but one complete one (a first re-verdict finds it)
    let renorm0 = d.join("renorm0");
    let o = run(&["renorm", "--plan", pdir.to_str().unwrap(), "--runs", runs.to_str().unwrap(), "--out", renorm0.to_str().unwrap(), "--profile", "cc_cohort_profile_v2",
        "--profile-population", "train2", "--split", "train", "--tokenizer", tok.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let left_out = jsonl(&renorm0.join("reverdict-split-train.jsonl")).into_iter().find(|i| i["complete_in_journals"] == true).unwrap()["item_id"].as_str().unwrap().to_string();
    let train_items: Vec<String> = jsonl(&pdir.join("items.jsonl")).iter().filter(|i| i["split"] == "train").map(|i| i["item_id"].as_str().unwrap().to_string()).collect();
    let records = d.join("records.txt");
    std::fs::write(&records, train_items.iter().filter(|i| **i != left_out).map(|i| format!("{i}\n")).collect::<String>()).unwrap();
    let out = d.join("renorm");
    let o = run(&["renorm", "--plan", pdir.to_str().unwrap(), "--runs", runs.to_str().unwrap(), "--out", out.to_str().unwrap(), "--profile", "cc_cohort_profile_v2",
        "--profile-population", "train2", "--split", "train", "--tokenizer", tok.to_str().unwrap(), "--records", records.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rows = d.join("r0-rows.jsonl");
    r0_rows(&pdir, &rows);
    let summary = text::sha_hex(&std::fs::read(out.join("RENORM-split-train.json")).unwrap());
    let rows_sha = text::sha_hex(&std::fs::read(&rows).unwrap());
    let count_with = |out: &Path, name: &str, h: &str, rh: &str| {
        run(&["dryrun-count", "--batch", "local", "--label", "local-test", "--renorm-out", out.to_str().unwrap(), "--summary-sha256", h, "--r0-rows", rows.to_str().unwrap(),
            "--r0-rows-sha256", rh, "--code-commit", "test", "--out", d.join(format!("{name}.json")).to_str().unwrap(), "--ids-out", d.join(format!("{name}-ids.jsonl")).to_str().unwrap()])
    };
    let count = |out: &Path, name: &str, h: &str| count_with(out, name, h, &rows_sha);
    let o = count(&out, "c1", &summary);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rec: Value = serde_json::from_slice(&std::fs::read(d.join("c1.json")).unwrap()).unwrap();
    assert_eq!(rec["schema"], "slop_ninja_a_r2_reverdict_dry_run_counts_batch_v1");
    assert_eq!(rec["counts"]["mismatched_children"], 0, "{}", rec["counts"]["mismatches"]);
    assert!(rec["stopped"].is_null());
    // the children: accepted requests of complete R0 items, as figures.py counts them
    let mut want = 0;
    for it in jsonl(&out.join("reverdict-split-train.jsonl")) {
        if it["r0_record"] == true && it["complete_in_journals"] == true {
            want += it["requests"].as_array().unwrap().iter().filter(|q| q["accepted_in_journals"] == true).count();
        }
    }
    assert!(want > 0);
    assert_eq!(rec["children"]["children"], want, "{}", rec["children"]);
    assert_eq!(rec["counts"]["totals"]["all"]["children"], want);
    // the complete item left out of R0's records is not counted
    assert!(rec["children"]["items"]["items_not_r0_records"].as_u64().unwrap() >= 1, "{}", rec["children"]);
    let all_complete: usize = jsonl(&out.join("reverdict-split-train.jsonl")).iter().filter(|it| it["complete_in_journals"] == true)
        .map(|it| it["requests"].as_array().unwrap().iter().filter(|q| q["accepted_in_journals"] == true).count()).sum();
    assert!(all_complete > want, "the left-out item has counted children");
    for split in ["cal", "dev_iter", "dev_select", "test"] {
        assert_eq!(rec["inputs"]["r0_rows"]["lines_of_other_splits_skipped_unparsed"][split], 1, "{split}");
    }
    assert_eq!(rec["inputs"]["r0_rows"]["sha256"], rows_sha.as_str());
    // count 1 through the local wiring (`judge_code` under amendment 7's code): its code panics on every
    // accepted prose child's spaced hyphens, and the landed code judges them (this plan's generator writes span
    // children only; the spliced and rows tests and the frontier example's pin verdict flips and reason changes)
    let all = &rec["counts"]["totals"]["all"];
    let reasons: serde_json::Map<String, Value> = rec["counts"]["reasons"].as_object().unwrap().values().flat_map(|m| m.as_object().unwrap().clone()).collect();
    assert!(all["c1_a7_failed"].as_u64().unwrap_or(0) >= 1, "{all}");
    assert!(reasons.contains_key("c1_a7_failed:panic:conventions.rs:389"), "{reasons:?}");
    // R0 rows of another digest are refused before any line is read
    let o = count_with(&out, "bad0", &summary, &"0".repeat(64));
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("is not the bound"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!d.join("bad0.json").exists());
    for k in ["count4_accepted_wrapped_lines_over_budget", "count5_kept_unit_sharing_a_unit", "count6_placed_break_paragraph_under_8_words"] {
        assert!(rec["counts"]["gate"][k]["gate"].is_u64(), "{k}");
    }
    // the ids-only file holds no text
    let ids = std::fs::read_to_string(d.join("c1-ids.jsonl")).unwrap();
    for w in ["amber", "acorn", "bramble"] {
        assert!(!ids.contains(w));
    }
    // a summary of another digest, and a missing input flag, are refused
    let o = count(&out, "bad1", &"0".repeat(64));
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("is not the recorded"));
    let o = run(&["dryrun-count", "--batch", "local", "--label", "x", "--renorm-out", out.to_str().unwrap(), "--summary-sha256", &summary, "--code-commit", "t",
        "--out", d.join("bad2.json").to_str().unwrap(), "--ids-out", d.join("bad2-ids.jsonl").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--r0-rows"));
    let o = run(&["dryrun-count", "--batch", "local", "--label", "x", "--renorm-out", out.to_str().unwrap(), "--summary-sha256", &summary, "--r0-rows", rows.to_str().unwrap(),
        "--code-commit", "t", "--out", d.join("bad3.json").to_str().unwrap(), "--ids-out", d.join("bad3-ids.jsonl").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("--r0-rows-sha256"), "{}", String::from_utf8_lossy(&o.stderr));
    // a plan and journals holding deliberately unparsable dev_select and test lines: the same counts
    let p2 = d.join("plan-mixed");
    copy_dir(&pdir, &p2);
    let junk = NOT_TRAIN;
    append(&p2.join("items.jsonl"), junk);
    let rf = p2.join("requests").join(format!("{generator}.jsonl"));
    append(&rf, junk);
    let mut m2: Value = serde_json::from_slice(&std::fs::read(p2.join("plan.json")).unwrap()).unwrap();
    m2["items_sha256"] = json!(text::sha_hex(&std::fs::read(p2.join("items.jsonl")).unwrap()));
    m2["request_files"][&generator]["sha256"] = json!(text::sha_hex(&std::fs::read(&rf).unwrap()));
    std::fs::write(p2.join("plan.json"), serde_json::to_vec(&m2).unwrap()).unwrap();
    let runs2 = d.join("runs-mixed");
    copy_dir(&runs, &runs2);
    let jf = std::fs::read_dir(runs2.join(&generator)).unwrap().map(|e| e.unwrap().path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("journal-")).unwrap();
    append(&jf, b"{\"kind\":\"response\",\"split\":\"dev_select\",\"verdict\":\x00 never parsed\n{\"kind\":\"response\",\"split\":\"cal\",\"verdict\":\x00 never parsed\n");
    let out2 = d.join("renorm-mixed");
    let o = run(&["renorm", "--plan", p2.to_str().unwrap(), "--runs", runs2.to_str().unwrap(), "--out", out2.to_str().unwrap(), "--profile", "cc_cohort_profile_v2",
        "--profile-population", "train2", "--split", "train", "--tokenizer", tok.to_str().unwrap(), "--records", records.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = count(&out2, "c2", &text::sha_hex(&std::fs::read(out2.join("RENORM-split-train.json")).unwrap()));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let rec2: Value = serde_json::from_slice(&std::fs::read(d.join("c2.json")).unwrap()).unwrap();
    assert_eq!(rec2["counts"]["cells"], rec["counts"]["cells"]);
    assert_eq!(rec2["counts"]["reasons"], rec["counts"]["reasons"]);
    for split in ["cal", "dev_iter", "dev_select", "test"] {
        let base = rec["inputs"]["plan_lines_skipped_unparsed"]["items"][split].as_u64().unwrap_or(0);
        assert_eq!(rec2["inputs"]["plan_lines_skipped_unparsed"]["items"][split].as_u64(), Some(base + 1), "{split}: {}", rec2["inputs"]["plan_lines_skipped_unparsed"]);
    }
    // the merge checks every cell's children against the figures file
    let mut cells = serde_json::Map::new();
    for (k, c) in rec["counts"]["cells"].as_object().unwrap() {
        cells.insert(k.clone(), json!({"children": c["children"], "children_accepted": c.get("children_accepted").cloned().unwrap_or(json!(0))}));
    }
    let fig = d.join("figures.json");
    // index 60's figures record schema (B27's merge refuses another)
    let schema = "slop_ninja_a_r2_reverdict_dry_run_figures_v1";
    std::fs::write(&fig, serde_json::to_vec(&json!({"schema": schema, "cells": cells})).unwrap()).unwrap();
    let fh = text::sha_hex(&std::fs::read(&fig).unwrap());
    let merged = d.join("DRY-RUN-COUNTS.json");
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), "--figures", fig.to_str().unwrap(), "--figures-sha256", &fh, "--code-commit", "t", "--out", merged.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let m: Value = serde_json::from_slice(&std::fs::read(&merged).unwrap()).unwrap();
    assert_eq!(m["schema"], "slop_ninja_a_r2_reverdict_dry_run_counts_v1");
    assert_eq!(m["counts"]["totals"]["all"]["children"], want);
    assert_eq!(m["population_check"]["differences"], json!([]));
    // a batch counted twice, and a figures file whose cells differ, are refused
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), d.join("c2.json").to_str().unwrap(), "--figures", fig.to_str().unwrap(), "--figures-sha256", &fh,
        "--code-commit", "t", "--out", d.join("M2.json").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("given twice"), "{}", String::from_utf8_lossy(&o.stderr));
    let fig2 = d.join("figures2.json");
    std::fs::write(&fig2, b"{\"schema\":\"slop_ninja_a_r2_reverdict_dry_run_figures_v1\",\"cells\":{\"local|mixed|web\":{\"children\":999}}}").unwrap();
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), "--figures", fig2.to_str().unwrap(), "--figures-sha256", &text::sha_hex(&std::fs::read(&fig2).unwrap()),
        "--code-commit", "t", "--out", d.join("M3.json").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("differ"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!d.join("M3.json").exists());
    // a figures file that differs in one cell's accepted children only is refused
    let mut cells4 = cells.clone();
    let k0 = cells4.keys().next().unwrap().clone();
    let acc0 = cells4[&k0]["children_accepted"].as_u64().unwrap();
    cells4[&k0]["children_accepted"] = json!(acc0 + 1);
    let fig4 = d.join("figures4.json");
    std::fs::write(&fig4, serde_json::to_vec(&json!({"schema": schema, "cells": cells4})).unwrap()).unwrap();
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), "--figures", fig4.to_str().unwrap(), "--figures-sha256", &text::sha_hex(&std::fs::read(&fig4).unwrap()),
        "--code-commit", "t", "--out", d.join("M4.json").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("differ"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!d.join("M4.json").exists());
    // a figures file without the figures schema is refused
    let fig5 = d.join("figures5.json");
    std::fs::write(&fig5, serde_json::to_vec(&json!({"cells": cells})).unwrap()).unwrap();
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), "--figures", fig5.to_str().unwrap(), "--figures-sha256", &text::sha_hex(&std::fs::read(&fig5).unwrap()),
        "--code-commit", "t", "--out", d.join("M5.json").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("not a dry-run figures record"), "{}", String::from_utf8_lossy(&o.stderr));
    // --rerun and --not-counted (B27): the batch as a re-run of itself, and a
    // not-counted batch whose index-60 figures add one cell to the population
    let label = rec["batch"].as_str().unwrap().to_string();
    let wide = d.join("figures-wide.json");
    let mut wcells = cells.clone();
    wcells.insert("frontier|mixed|web".into(), json!({"children": 3, "children_accepted": 2}));
    std::fs::write(&wide, serde_json::to_vec(&json!({"schema": schema, "cells": wcells})).unwrap()).unwrap();
    let w1 = d.join("figures-w1.json");
    std::fs::write(&w1, serde_json::to_vec(&json!({"schema": schema, "cells": {"frontier|mixed|web": {"children": 3, "children_accepted": 2}}})).unwrap()).unwrap();
    let (wh, w1h) = (text::sha_hex(&std::fs::read(&wide).unwrap()), text::sha_hex(&std::fs::read(&w1).unwrap()));
    let out6 = d.join("M6.json");
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), "--figures", wide.to_str().unwrap(), "--figures-sha256", &wh,
        "--rerun", &label, fig.to_str().unwrap(), &fh, fig.to_str().unwrap(), &fh, "--not-counted", "frontier-wave1", w1.to_str().unwrap(), &w1h,
        "--code-commit", "t", "--out", out6.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let m: Value = serde_json::from_slice(&std::fs::read(&out6).unwrap()).unwrap();
    assert_eq!((m["scope"].as_str(), m["not_counted"][0]["batch"].as_str(), m["rerun_batches"][0]["batch"].as_str()), (Some("complete except frontier-wave1"), Some("frontier-wave1"), Some(label.as_str())));
    assert_eq!(m["counts"]["totals"]["all"]["children"], want);
    // without --not-counted the wider population is refused
    let o = run(&["dryrun-merge", "--batch", d.join("c1.json").to_str().unwrap(), "--figures", wide.to_str().unwrap(), "--figures-sha256", &wh,
        "--code-commit", "t", "--out", d.join("M7.json").to_str().unwrap()]);
    assert!(!o.status.success() && String::from_utf8_lossy(&o.stderr).contains("differ"), "{}", String::from_utf8_lossy(&o.stderr));
}

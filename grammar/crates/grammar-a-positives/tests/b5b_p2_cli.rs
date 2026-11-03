//! B5b part 2 (BUILD-PLAN 11.4; ruling W6-R1) run as the binary on
//! synthetic text (no corpus text, no model or network call beyond a
//! loopback stand-in server):
//! - the Train-only mirror planner (`train-mirror-plan`): Train lines only,
//!   no `--dev-select`, `mirror_target_words_v2` with r05 from S0-6 (refused
//!   while absent), one split, run by `generate --profile` and `finalize`;
//! - D6's plan (`enron-plan`): enron-half's Train file only, a diagnostic-half
//!   key refused, counts and words against S0-9;
//! - the top-up plan (`topup-plan`, `topup-check`): S0-8's rules filled from
//!   Train-mining or recorded as shortfalls, the frontier label allocation and
//!   its check, D5's mirror plan on its parents;
//! - D8's pilot on a spliced plan (`pilot --population --renorm`) and the
//!   spliced generator's `--request-order`;
//! - S0-10 (`stratify-train --write-stratification`, `--stratification`).
use grammar_a_positives::splice::{self, Edit, Item as SItem};
use grammar_a_positives::{plan, text};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_a_positives");
const SPLICED: &str = env!("CARGO_BIN_EXE_slop_ninja_a_spliced");

const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
    "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
];
const ALT: [&str; 20] = [
    "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
    "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
];

/// Old Dev-select and consumed-Test lines whose split key is readable and
/// whose rest is not JSON: a reader that parses one fails loudly.
const JUNK_FS: &[u8] = b"{\"frozen_split\":\"dev_select\",\"id\":\"x\",\x00 never parsed\n{\"id\":\"t\", \"frozen_split\" : \"test\", broken\n";

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

/// A synthetic human row of `sentences` sentences of 11 words.
fn human(id: &str, split: &str, coll: &str, seed: u64, sentences: usize) -> Value {
    let t = format!("{}\n\n{}", prose(&SYN, seed, sentences / 2, 11), prose(&SYN, seed + 7919, sentences - sentences / 2, 11));
    json!({"id": id, "frozen_split": split, "origin": "human_only", "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "word_count": text::words(&t),
        "collection": coll, "provenance": {"title": format!("synthetic {id}")}, "source_group": format!("g-{id}"), "domain": "web"})
}

fn write_rows(p: &Path, rows: &[Value]) {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r).unwrap();
        b.push(b'\n');
    }
    std::fs::write(p, b).unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> Value {
    assert!(o.status.success(), "{}", err(o));
    serde_json::from_slice(&o.stdout).unwrap_or(Value::Null)
}

fn refused(o: &Output, want: &str) {
    assert!(!o.status.success() && err(o).contains(want), "want {want:?}: {}", err(o));
}

fn jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn sha_file(p: &Path) -> String {
    text::sha_hex(&std::fs::read(p).unwrap())
}

fn append(p: &Path, b: &[u8]) {
    std::fs::OpenOptions::new().append(true).open(p).unwrap().write_all(b).unwrap();
}

fn values_file(d: &Path, name: &str, entries: Value) -> PathBuf {
    let p = d.join(name);
    std::fs::write(&p, serde_json::to_vec(&json!({"schema": "slop_ninja_a_round2_values_v1", "values": entries})).unwrap()).unwrap();
    p
}

fn s06(r: &str) -> Value {
    json!({"id": "S0-6", "name": "r05", "value": r})
}

fn tokenizer(d: &Path) -> PathBuf {
    let tok = d.join("tok.json");
    std::fs::write(&tok, r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#).unwrap();
    tok
}

fn index(d: &Path) -> PathBuf {
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

/// A llama-server stand-in: `/props`, `/v1/models`, and for every chat call
/// a distinct prose reply of `words` words (11 per sentence).
fn serve(words: usize) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        let mut n = 0u64;
        for st in listener.incoming() {
            let mut st = st.unwrap();
            let Some(line) = read_http(&mut st) else { continue };
            let out = if line.starts_with("QUIT") {
                break;
            } else if line.starts_with("GET /props") {
                json!({"model_path": "/models/m.gguf", "chat_template": "t"})
            } else if line.starts_with("GET /v1/models") {
                json!({"data": [{"id": "m"}]})
            } else {
                n += 1;
                json!({"choices":[{"message":{"content":prose(&ALT, 1000 + n, words / 11, 11)},"finish_reason":"stop"}],"usage":{},"timings":{}})
            };
            let out = serde_json::to_vec(&out).unwrap();
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
            st.write_all(head.as_bytes()).unwrap();
            st.write_all(&out).unwrap();
        }
    });
    (format!("http://{addr}"), h)
}

fn read_http(s: &mut TcpStream) -> Option<String> {
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
    Some(head.lines().next()?.to_owned())
}

fn quit(endpoint: &str) {
    TcpStream::connect(endpoint.trim_start_matches("http://")).unwrap().write_all(b"QUIT / HTTP/1.1\r\n\r\n").unwrap();
}

/// Every request of a plan directory, by request id.
fn plan_requests(pdir: &Path) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    for e in std::fs::read_dir(pdir.join("requests")).unwrap() {
        for r in jsonl(&e.unwrap().path()) {
            m.insert(r["request_id"].as_str().unwrap().to_string(), r);
        }
    }
    m
}

/// Family counts of a plan's items, per domain.
fn family_counts(pdir: &Path) -> BTreeMap<String, BTreeMap<String, usize>> {
    let mut m: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for it in jsonl(&pdir.join("items.jsonl")) {
        *m.entry(it["domain"].as_str().unwrap().into()).or_default().entry(it["generator"].as_str().unwrap().into()).or_default() += 1;
    }
    m
}

/// The equal split of `n` over the seven families, in FAMILY_ORDER (zeros dropped).
fn equal(n: usize) -> BTreeMap<String, usize> {
    plan::FAMILY_ORDER.iter().enumerate().map(|(i, f)| (f.to_string(), n / 7 + usize::from(i < n % 7))).filter(|(_, c)| *c > 0).collect()
}

/// An R0-like local plan v1 directory for `--cal-from`: its plan.json
/// records a train input that does not exist and a CAL input. With `valid`
/// the CAL file holds two synthetic CAL humans; otherwise it does not exist
/// (the planner never opens it).
fn cal_source(d: &Path, name: &str, valid: bool) -> PathBuf {
    let src = d.join(name);
    std::fs::create_dir_all(&src).unwrap();
    let cal = src.join("cal.jsonl");
    let sha = if valid {
        write_rows(&cal, &(0..2).map(|i| human(&format!("cal-{i}"), "cal", "wikipedia_enwiki_historical", 500 + i, 15)).collect::<Vec<_>>());
        sha_file(&cal)
    } else {
        "ab".repeat(32)
    };
    std::fs::write(src.join("plan.json"), serde_json::to_vec(&json!({"schema": "slop_ninja_a_comp_positive_plan_v1",
        "inputs": {"train": {"path": "/nonexistent/train.jsonl", "sha256": "cd".repeat(32)}, "dev_select": {"path": "/nonexistent/ds.jsonl", "sha256": "ef".repeat(32)},
            "cal": {"path": cal, "sha256": sha}}})).unwrap()).unwrap();
    src
}

/// Each item's decoding is `plan::train_decoding` of its parent's rank of
/// V21 key `train_mirror_decoding` within (train, domain), spelled out
/// independently of the planner.
fn assert_decoding_ranks(pdir: &Path) {
    let items = jsonl(&pdir.join("items.jsonl"));
    let mut by: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for it in &items {
        let pid = it["parent_id"].as_str().unwrap().to_string();
        by.entry(it["domain"].as_str().unwrap().into()).or_default().push((key("train_mirror_decoding", &pid), pid));
    }
    let mut rank: BTreeMap<String, usize> = BTreeMap::new();
    for (_, mut v) in by {
        v.sort();
        for (r, (_, id)) in v.into_iter().enumerate() {
            rank.insert(id, r);
        }
    }
    let reqs = plan_requests(pdir);
    for it in &items {
        let want = serde_json::to_value(plan::train_decoding(rank[it["parent_id"].as_str().unwrap()])).unwrap();
        assert_eq!(it["decoding"], want, "{}", it["item_id"]);
        assert_eq!(reqs[it["requests"][0].as_str().unwrap()]["decoding"], want);
    }
}

#[test]
fn train_mirror_plan_parses_train_lines_only_and_its_plan_runs_under_generate_and_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let tr = d.join("train.jsonl");
    // 9 fineweb humans of 165 words and 5 wikipedia humans of 209 words, then old Dev-select and Test lines
    let mut rows: Vec<Value> = (0..9).map(|i| human(&format!("fw-{i}"), "train", "fineweb_cc_historical", i, 15)).collect();
    rows.extend((0..5).map(|i| human(&format!("wp-{i}"), "train", "wikipedia_enwiki_historical", 100 + i, 19)));
    write_rows(&tr, &rows);
    let vals = values_file(d, "values.json", json!({"S0-6": s06("0.8")}));
    let calsrc = cal_source(d, "r0-plan", true);
    let cf = s(&calsrc);
    // old Dev-select and Test lines are never parsed: the input is refused at the first, by line number
    let junk = d.join("train-junk.jsonl");
    std::fs::copy(&tr, &junk).unwrap();
    append(&junk, JUNK_FS);
    let o = run(&["train-mirror-plan", "--train", s(&junk), "--values", s(&vals), "--cal-from", cf, "--out", s(&d.join("pj"))]);
    refused(&o, "line 15: names a refused split (old Dev-select or the consumed Test) (dev_select)");
    assert!(!err(&o).contains("never parsed") && !err(&o).contains("key must be a string") && !d.join("pj").exists(), "{}", err(&o));
    let junk_t = d.join("train-junk-test.jsonl");
    std::fs::copy(&tr, &junk_t).unwrap();
    append(&junk_t, &JUNK_FS[JUNK_FS.iter().position(|b| *b == b'\n').unwrap() + 1..]);
    refused(&run(&["train-mirror-plan", "--train", s(&junk_t), "--values", s(&vals), "--cal-from", cf, "--out", s(&d.join("pj2"))]), "line 15: names a refused split (old Dev-select or the consumed Test) (test)");
    let other = d.join("train-other.jsonl");
    std::fs::copy(&tr, &other).unwrap();
    append(&other, &serde_json::to_vec(&human("dev-0", "dev_iter", "fineweb_cc_historical", 9, 15)).unwrap());
    refused(&run(&["train-mirror-plan", "--train", s(&other), "--values", s(&vals), "--cal-from", cf, "--out", s(&d.join("pj3"))]), "line 15: names another plan split (dev_iter)");
    // there is no --dev-select, and nothing is written
    let o = run(&["train-mirror-plan", "--train", s(&tr), "--dev-select", s(&tr), "--values", s(&vals), "--cal-from", cf, "--out", s(&d.join("p0"))]);
    refused(&o, "--dev-select");
    assert!(!d.join("p0").exists());
    // r05 is S0-6's: refused while it is absent
    let no6 = values_file(d, "values-no-s06.json", json!({"S0-7": {"id": "S0-7", "name": "v_fam", "value": 5}}));
    let o = run(&["train-mirror-plan", "--train", s(&tr), "--values", s(&no6), "--cal-from", cf, "--out", s(&d.join("p1"))]);
    refused(&o, "holds no S0-6");
    assert!(!d.join("p1").exists());
    // R0's CAL overlap extra is required: no --cal-from, a source that is not an R0 local plan, or one without a CAL input
    refused(&run(&["train-mirror-plan", "--train", s(&tr), "--values", s(&vals), "--out", s(&d.join("pc0"))]), "--cal-from");
    let bad_src = d.join("not-r0");
    std::fs::create_dir_all(&bad_src).unwrap();
    std::fs::write(bad_src.join("plan.json"), serde_json::to_vec(&json!({"schema": "slop_ninja_a_comp_positive_plan_v3", "inputs": {"cal": {"path": "/x", "sha256": "ab".repeat(32)}}})).unwrap()).unwrap();
    refused(&run(&["train-mirror-plan", "--train", s(&tr), "--values", s(&vals), "--cal-from", s(&bad_src), "--out", s(&d.join("pc1"))]), "not an R0 local plan v1 or v2");
    std::fs::write(bad_src.join("plan.json"), serde_json::to_vec(&json!({"schema": "slop_ninja_a_comp_positive_plan_v2", "inputs": {"train": {"path": "/x", "sha256": "ab".repeat(32)}}})).unwrap()).unwrap();
    refused(&run(&["train-mirror-plan", "--train", s(&tr), "--values", s(&vals), "--cal-from", s(&bad_src), "--out", s(&d.join("pc2"))]), "records no CAL input");
    assert!(!d.join("pc0").exists() && !d.join("pc1").exists() && !d.join("pc2").exists());
    // the CAL record is copied from the source plan unchanged; the planner opens no CAL file (this one does not exist)
    let ghost = cal_source(d, "r0-plan-ghost", false);
    let pc = d.join("plan-cal");
    let r = ok(&run(&["train-mirror-plan", "--train", s(&tr), "--values", s(&vals), "--cal-from", s(&ghost), "--out", s(&pc)]));
    assert_eq!(r["items"], 14);
    let mc = read_json(&pc.join("plan.json"));
    assert_eq!(mc["inputs"].as_object().unwrap().keys().collect::<Vec<_>>(), ["cal", "train"]);
    assert_eq!(mc["inputs"]["cal"], read_json(&ghost.join("plan.json"))["inputs"]["cal"]);
    assert_eq!(mc["train_mirror"]["cal_from"]["sha256"], sha_file(&ghost.join("plan.json")));
    // the plan
    let pdir = d.join("plan");
    ok(&run(&["train-mirror-plan", "--train", s(&tr), "--values", s(&vals), "--cal-from", cf, "--out", s(&pdir)]));
    let m = read_json(&pdir.join("plan.json"));
    assert_eq!(m["schema"], "slop_ninja_a_comp_positive_plan_train_mirror_v1");
    assert_eq!(m["inputs"].as_object().unwrap().keys().collect::<Vec<_>>(), ["cal", "train"]);
    assert_eq!(m["inputs"]["train"]["sha256"], sha_file(&tr));
    assert_eq!(m["inputs"]["cal"]["sha256"], sha_file(&calsrc.join("cal.jsonl")));
    assert_eq!(m["items_sha256"], sha_file(&pdir.join("items.jsonl")));
    assert_eq!(m["train_mirror"]["train_input"], json!({"lines": 14, "one_split": "train"}));
    assert_eq!((m["train_mirror"]["values"]["sha256"].as_str(), m["train_mirror"]["values"]["value"].as_str()), (Some(sha_file(&vals).as_str()), Some("0.8")));
    assert_eq!(m["train_mirror"]["mirror_target"]["floor_words"], 188);
    assert_eq!(m["humans"]["train"], json!({"fineweb": 9, "wikipedia": 5}));
    // one mirror per human, at max(parent words, ceil(150 / 0.8) = 188), one split
    let items = jsonl(&pdir.join("items.jsonl"));
    let reqs = plan_requests(&pdir);
    assert_eq!((items.len(), reqs.len()), (14, 14));
    let words: BTreeMap<String, u64> = rows.iter().map(|r| (r["id"].as_str().unwrap().to_string(), r["word_count"].as_u64().unwrap())).collect();
    assert_eq!(words.values().copied().collect::<BTreeSet<_>>(), [165, 209].into());
    let parents: BTreeSet<&str> = items.iter().map(|i| i["parent_id"].as_str().unwrap()).collect();
    assert_eq!(parents.len(), 14);
    for it in &items {
        assert_eq!((it["kind"].as_str(), it["cohort"].as_str(), it["split"].as_str()), (Some("mirror"), Some("train2_mirror"), Some("train")));
        let r = &reqs[it["requests"][0].as_str().unwrap()];
        let want = words[it["parent_id"].as_str().unwrap()].max(188);
        assert_eq!(r["target_words"].as_u64(), Some(want));
        assert!(r["prompt"].as_str().unwrap().contains(&format!(" in English, about {want} words long.")));
        assert!(r.get("attempt_limit").is_none());
        assert!(plan::FAMILY_ORDER.contains(&r["generator"].as_str().unwrap()));
    }
    // the seven families, split equally within each domain; decoding by the train_mirror_decoding key
    let fc = family_counts(&pdir);
    assert_eq!((fc["fineweb"].clone(), fc["wikipedia"].clone()), (equal(9), equal(5)));
    assert_decoding_ranks(&pdir);
    // a line whose split cannot be read refuses the plan by line number
    let bad = d.join("train-bad.jsonl");
    std::fs::copy(&tr, &bad).unwrap();
    append(&bad, b"{\"id\":\"no-split\"}\n");
    let o = run(&["train-mirror-plan", "--train", s(&bad), "--values", s(&vals), "--cal-from", cf, "--out", s(&d.join("p2"))]);
    refused(&o, "line 15: no frozen_split");
    assert!(!d.join("p2").exists());
    // the keyed pilot reads the plan (a cell below 59 rows: piloted whole)
    ok(&run(&["pilot", "--plan", s(&pdir), "--step", "D5", "--out", s(&d.join("pilot"))]));
    let pj = read_json(&d.join("pilot").join("PILOT.json"));
    assert_eq!((pj["rows"].as_u64(), pj["whole_step_is_pilot"].as_bool()), (Some(14), Some(true)));
    assert_eq!(pj["cells"]["fineweb|model_only"]["rows"], 9);

    // generate --profile and finalize accept the plan; profile v2's window takes the 198-word mirrors
    let g = "qwen36";
    let (endpoint, server) = serve(198);
    let configs = d.join("configs");
    std::fs::create_dir_all(&configs).unwrap();
    let cfg = configs.join(format!("{g}.json"));
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": g, "family": "Qwen synthetic", "allowed_splits": ["train"], "endpoint": endpoint, "model_path": "/models/m.gguf", "alias": "m"})).unwrap()).unwrap();
    let (idx, tok) = (index(d), tokenizer(d));
    let runs = d.join("runs");
    let gen_run = |extra: &[&str]| {
        Command::new(BIN)
            .args(["generate", "--plan", s(&pdir), "--generator", g, "--config", s(&cfg), "--runs", s(&runs), "--index", s(&idx), "--tokenizer", s(&tok)])
            .args(["--workers", "1", "--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"])
            .args(extra)
            .output()
            .unwrap()
    };
    let dry = ok(&gen_run(&["--dry-run"]));
    let n_g = items.iter().filter(|i| i["generator"] == g).count();
    assert_eq!((dry["would_run_items"].as_u64(), dry["profile"]["population"].as_str()), (Some(n_g as u64), Some("train2")));
    ok(&gen_run(&[]));
    quit(&endpoint);
    server.join().unwrap();
    let fin = d.join("fin");
    ok(&run(&["finalize", "--plan", s(&pdir), "--runs", s(&runs), "--configs", s(&configs), "--tokenizer", s(&tok), "--out", s(&fin)]));
    let recs = jsonl(&fin.join("train").join("records.jsonl"));
    assert_eq!(recs.len(), n_g);
    assert!(recs.iter().all(|r| r["cohort"] == "train2_mirror" && r["origin"] == "model_only" && r["kind"] == "mirror"));
    assert!(recs.iter().all(|r| (150..=450).contains(&(r["word_count"].as_u64().unwrap() as usize))));
}

/// A synthetic enron-v1 pool under `root` and its S0-9 record: 99 admitted
/// writer keys (buckets below 900) with two documents each of `sentences`
/// sentences, 3 reserved keys, the pool files and their SHA256SUMS.
fn enron_fixture(root: &Path, sentences: usize) -> (PathBuf, Value, Vec<String>, Vec<String>) {
    use grammar_a_pools::{adapter, pile};
    let keys = |n: usize, want: &dyn Fn(u32) -> bool| -> Vec<String> {
        let mut v = Vec::new();
        let mut i = 0;
        while v.len() < n {
            let k = pile::contributor_key("enron_mailbox", &format!("synthetic-mailer-{i}"));
            if want(adapter::bucket(&k)) {
                v.push(k);
            }
            i += 1;
        }
        v
    };
    let admitted = keys(99, &|b| b < 900);
    let mut reserved = keys(3, &|b| b >= 900);
    reserved.sort();
    let mut order = admitted.clone();
    order.sort_by_key(|k| pile::enron_half_plan_key(k));
    let (train, diag) = order.split_at(50);
    let pool = root.join("pools").join("enron-v1");
    std::fs::create_dir_all(&pool).unwrap();
    let mut docs = Vec::new();
    let mut words = 0usize;
    for (i, k) in admitted.iter().enumerate() {
        for j in 0..2 {
            let t = prose(&SYN, (i * 2 + j) as u64, sentences, 11);
            if train.contains(k) {
                words += text::words(&t);
            }
            let row = json!({"id": format!("pool-enron-{i}-{j}"), "collection": "enron_email_historical", "domain": "email_corporate", "writer_key": k,
                "origin": "human_only", "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "word_count": text::words(&t), "source_group": format!("g-{i}-{j}"),
                "provenance": {"subject": format!("synthetic subject {i}")}});
            docs.extend_from_slice(serde_json::to_string(&row).unwrap().as_bytes());
            docs.push(b'\n');
        }
    }
    std::fs::write(pool.join("documents.jsonl"), &docs).unwrap();
    for f in ["ADMISSION.json", "metadata.jsonl", "reserved-metadata.jsonl"] {
        std::fs::write(pool.join(f), format!("synthetic {f}\n")).unwrap();
    }
    let sums: String = ["ADMISSION.json", "documents.jsonl", "metadata.jsonl", "reserved-metadata.jsonl"]
        .iter()
        .map(|f| format!("{}  ./{f}\n", sha_file(&pool.join(f))))
        .collect();
    std::fs::write(pool.join("SHA256SUMS"), &sums).unwrap();
    let list = |ks: &[String]| text::sha_hex(ks.iter().map(|k| format!("{k}\n")).collect::<String>().as_bytes());
    let input = |f: &str| json!({"path": format!("pools/enron-v1/{f}"), "sha256": sha_file(&pool.join(f)), "bytes": std::fs::metadata(pool.join(f)).unwrap().len()});
    let s09 = json!({"id": "S0-9", "name": "enron_halves", "plan_key_prefix": "slop_ninja_a_comp_r2_plan_v1:enron_half:",
        "train_half": {"writer_keys_in_plan_key_order": train, "writer_keys": 50, "documents": 100, "words": words, "list_sha256": list(train)},
        "diagnostic_half": {"writer_keys_in_plan_key_order": diag, "writer_keys": 49, "documents": 98, "list_sha256": list(diag)},
        "reserved_mailboxes_diagnostic": {"owner_keys_sorted": reserved, "owner_keys": 3, "list_sha256": list(&reserved)},
        "checks": {"writer_keys": 99, "reserved_owner_keys": 3},
        "inputs": [input("ADMISSION.json"), input("metadata.jsonl"), input("reserved-metadata.jsonl"), input("SHA256SUMS")]});
    (pool, s09, train.to_vec(), diag.to_vec())
}

#[test]
fn enron_plan_reads_the_train_half_only_and_refuses_a_diagnostic_key() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let root = d.join("repo");
    let (pool, s09, _train, diag) = enron_fixture(&root, 15);
    let vals = values_file(d, "values.json", json!({"S0-9": s09, "S0-6": s06("0.8")}));
    let half = d.join("train-half.jsonl");
    ok(&run(&["enron-half", "--pool", s(&pool), "--root", s(&root), "--values", s(&vals), "--out", s(&half)]));
    let calsrc = cal_source(d, "r0-plan", true);
    let cf = s(&calsrc);
    let pdir = d.join("d6");
    refused(&run(&["enron-plan", "--train-half", s(&half), "--values", s(&vals), "--out", s(&d.join("d6-nocal"))]), "--cal-from");
    let r = ok(&run(&["enron-plan", "--train-half", s(&half), "--values", s(&vals), "--cal-from", cf, "--out", s(&pdir)]));
    assert_eq!((r["items"].as_u64(), r["cohort"].as_str()), (Some(100), Some("train2_enron_mirror")));
    let m = read_json(&pdir.join("plan.json"));
    assert_eq!(m["humans"]["train"], json!({"enron_email": 100}));
    assert_eq!(m["train_mirror"]["source"]["step"], "D6");
    assert_eq!(m["train_mirror"]["source"]["train_half"]["sha256"], sha_file(&half));
    // one mirror per document, the seven Train families equally, the Enron register, mirror_target_words_v2
    let items = jsonl(&pdir.join("items.jsonl"));
    assert_eq!(items.iter().map(|i| i["parent_id"].as_str().unwrap()).collect::<BTreeSet<_>>().len(), 100);
    assert_eq!(family_counts(&pdir)["enron_email"], equal(100));
    for r in plan_requests(&pdir).values() {
        assert!(r["prompt"].as_str().unwrap().starts_with("Write an email message written by an employee of a company in English, about 188 words long."), "{}", r["prompt"]);
    }
    assert!(!String::from_utf8_lossy(&std::fs::read(pdir.join("plan.json")).unwrap()).contains("enron_mailbox:"));
    assert_eq!(m["inputs"]["cal"], read_json(&calsrc.join("plan.json"))["inputs"]["cal"]);
    // generate reads the Enron Train rows of this Train-only plan (its prior: the plan's parents, population train2)
    let g = "qwen36";
    let configs = d.join("configs");
    std::fs::create_dir_all(&configs).unwrap();
    let cfg = configs.join(format!("{g}.json"));
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": g, "family": "Qwen synthetic", "allowed_splits": ["train"], "endpoint": "http://127.0.0.1:9", "model_path": "/models/m.gguf", "alias": "m"})).unwrap()).unwrap();
    let (idx, tok) = (index(d), tokenizer(d));
    let dry = ok(&Command::new(BIN)
        .args(["generate", "--plan", s(&pdir), "--generator", g, "--config", s(&cfg), "--runs", s(&d.join("runs")), "--index", s(&idx), "--tokenizer", s(&tok)])
        .args(["--workers", "1", "--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2", "--dry-run"])
        .output()
        .unwrap());
    assert_eq!((dry["would_run_items"].as_u64(), dry["profile"]["population"].as_str()), (Some(equal(100)[g] as u64), Some("train2")));
    // and finalize reads them (no response yet: no record)
    std::fs::create_dir_all(d.join("runs")).unwrap();
    ok(&run(&["finalize", "--plan", s(&pdir), "--runs", s(&d.join("runs")), "--configs", s(&configs), "--tokenizer", s(&tok), "--out", s(&d.join("fin6"))]));
    // every other Train reader keeps plan::domain_of, which refuses the rows (plan-v3 below; plan.rs's unit test)
    // the bindings, each refused before any plan is written and with no key printed
    let lines: Vec<String> = std::fs::read_to_string(&half).unwrap().lines().map(String::from).collect();
    let refuse = |name: &str, body: String, values: &Path, want: &str| {
        let p = d.join(format!("{name}.jsonl"));
        std::fs::write(&p, body).unwrap();
        let out = d.join(format!("out-{name}"));
        let o = run(&["enron-plan", "--train-half", s(&p), "--values", s(values), "--cal-from", cf, "--out", s(&out)]);
        refused(&o, want);
        assert!(!out.exists() && !err(&o).contains("enron_mailbox:"), "{name}");
    };
    let join = |ls: &[String]| ls.iter().map(|l| format!("{l}\n")).collect::<String>();
    let k0 = serde_json::from_str::<Value>(&lines[0]).unwrap()["writer_key"].as_str().unwrap().to_string();
    let mut ls = lines.clone();
    ls[0] = ls[0].replace(&k0, &diag[0]);
    refuse("diag-key", join(&ls), &vals, "a diagnostic-half writer key");
    let mut ls = lines.clone();
    ls[0] = ls[0].replace(&k0, &grammar_a_pools::pile::contributor_key("enron_mailbox", "synthetic-stranger"));
    refuse("other-key", join(&ls), &vals, "outside S0-9's Train half");
    let mut ls = lines.clone();
    ls[1] = ls[1].replace("enron_email_historical", "fineweb_cc_historical");
    refuse("collection", join(&ls), &vals, "not an enron_email_historical document");
    refuse("short", join(&lines[1..]), &vals, "documents, S0-9 records another count");
    // the same documents with one word fewer in one of them: S0-9's words differ
    let mut v: Value = serde_json::from_str(&lines[3]).unwrap();
    let t = v["text"].as_str().unwrap().split_once(' ').unwrap().1.to_string();
    v["word_count"] = json!(text::words(&t));
    v["text_sha256"] = json!(text::sha_hex(t.as_bytes()));
    v["text"] = json!(t);
    let mut ls = lines.clone();
    ls[3] = serde_json::to_string(&v).unwrap();
    refuse("words", join(&ls), &vals, "words, S0-9 records another count");
    let mut v: Value = serde_json::from_str(&lines[2]).unwrap();
    v.as_object_mut().unwrap().remove("split_rule");
    let mut ls = lines.clone();
    ls[2] = serde_json::to_string(&v).unwrap();
    refuse("no-rule", join(&ls), &vals, "not written by enron-half");
    let only9 = values_file(d, "values-s09.json", json!({"S0-9": read_json(&vals)["values"]["S0-9"]}));
    refuse("no-s06", join(&lines), &only9, "holds no S0-6");
    // the raw pool documents are not the Train half
    let o = run(&["enron-plan", "--train-half", s(&pool.join("documents.jsonl")), "--values", s(&vals), "--cal-from", cf, "--out", s(&d.join("out-pool"))]);
    assert!(!o.status.success() && !d.join("out-pool").exists(), "{}", err(&o));
    // plans v1 to v3 refuse Enron rows as they did before the domain was mapped
    let o = run(&["plan-v3", "--train", s(&half), "--out", s(&d.join("v3"))]);
    refused(&o, "unadmitted collection enron_email_historical");
    let o = run(&["plan-v3", "--train", s(&half), "--base-generators", "granite_base", "--out", s(&d.join("v3b"))]);
    refused(&o, "unadmitted collection enron_email_historical");
}

/// The top-up fixture: Train-mining documents and manifest, R0's Train and
/// manifest, the seven family configs and the collector config.
struct Topup {
    tm: PathBuf,
    tmm: PathBuf,
    r0: PathBuf,
    r0m: PathBuf,
    configs: PathBuf,
    fc: PathBuf,
}

fn topup_fixture(d: &Path) -> Topup {
    let mut tm: Vec<Value> = Vec::new();
    let mut man: Vec<Value> = Vec::new();
    let mut add_row = |row: Value, dom: Option<&str>| {
        if let Some(dm) = dom {
            man.push(json!({"id": row["id"], "split": "train_mining", "domain": dm, "source_group": row["source_group"]}));
        }
        tm.push(row);
    };
    let mut add = |id: String, coll: &str, dom: Option<&str>, sentences: usize, seed: u64| add_row(human(&id, "train_mining", coll, seed, sentences), dom);
    for i in 0..12 {
        add(format!("cap-{i}"), "caselaw_access_project_historical", Some("caselaw_access_project"), 15, i);
    }
    for i in 0..2 {
        add(format!("cap-short-{i}"), "caselaw_access_project_historical", Some("caselaw_access_project"), 4, 50 + i);
    }
    add("cap-long".into(), "caselaw_access_project_historical", Some("caselaw_access_project"), 42, 60);
    for i in 0..3 {
        add(format!("cl-{i}"), "courtlistener_opinions_historical", Some("courtlistener"), 15, 70 + i);
    }
    add("tm-enron-row".into(), "enron_email_historical", Some("caselaw_access_project"), 15, 80);
    add("tm-enron-domain".into(), "fineweb_cc_historical", Some("enron_email"), 15, 81);
    add("tm-no-manifest".into(), "caselaw_access_project_historical", None, 15, 82);
    add("tm-unplannable".into(), "mystery_archive_historical", Some("caselaw_access_project"), 15, 83);
    add("wp-0".into(), "wikipedia_enwiki_historical", Some("wikipedia"), 15, 84);
    // the window's bounds: 150 and 450 words are inside, 149 and 451 outside
    for (w, seed) in [(149usize, 90u64), (150, 91), (450, 92), (451, 93)] {
        add_row(human_w(&format!("cap-w{w}"), "train_mining", "caselaw_access_project_historical", seed, w), Some("caselaw_access_project"));
    }
    let tm_p = d.join("tm.jsonl");
    write_rows(&tm_p, &tm);
    append(&tm_p, JUNK_FS);
    // a Train-mining file that repeats an id is refused
    let dup = d.join("tm-dup.jsonl");
    write_rows(&dup, &[tm[0].clone(), tm[0].clone()]);
    let tmm = d.join("tm-manifest.jsonl");
    write_rows(&tmm, &man);
    append(&tmm, b"{\"split\":\"dev_select\",\"id\":\"x\",\x00 never parsed\n");
    // R0's Train: 8 wikipedia humans in the window (wp-0 is also listed in Train-mining) and two outside it
    let mut r0: Vec<Value> = (0..8).map(|i| human(&format!("wp-{i}"), "train", "wikipedia_enwiki_historical", 200 + i, 15)).collect();
    r0.push(human("wp-short", "train", "wikipedia_enwiki_historical", 220, 4));
    r0.push(human("wp-long", "train", "wikipedia_enwiki_historical", 221, 42));
    let r0_p = d.join("r0.jsonl");
    write_rows(&r0_p, &r0);
    let r0m = d.join("r0-manifest.jsonl");
    write_rows(&r0m, &r0.iter().map(|r| json!({"id": r["id"], "split": "train", "domain": "wikipedia", "source_group": r["source_group"]})).collect::<Vec<_>>());
    let configs = d.join("configs");
    std::fs::create_dir_all(&configs).unwrap();
    for f in plan::FAMILY_ORDER {
        std::fs::write(configs.join(format!("{f}.json")), serde_json::to_vec(&json!({"key": f, "family": format!("Family {f}")})).unwrap()).unwrap();
    }
    std::fs::write(configs.join("gptoss20b.json"), serde_json::to_vec(&json!({"key": "gptoss20b", "family": "Family gptoss20b"})).unwrap()).unwrap();
    let fc = d.join("frontier-config.json");
    let labels = ["gpt-6-astra", "gpt-6-1-sol", "gpt-6-1-terra", "gpt-6-sol", "gpt-5-6-terra"];
    std::fs::write(&fc, serde_json::to_vec(&json!({"schema": "slop_ninja_frontier_plan_config_v1", "models": labels.iter().map(|l| json!({"label": l, "transport": "codex-cli"})).collect::<Vec<_>>()})).unwrap()).unwrap();
    Topup { tm: tm_p, tmm, r0: r0_p, r0m, configs, fc }
}

/// A synthetic human row of exactly `words` words (sentences of up to 11).
fn human_w(id: &str, split: &str, coll: &str, seed: u64, words: usize) -> Value {
    let mut parts = Vec::new();
    let (mut left, mut k) = (words, 0u64);
    while left > 0 {
        let n = left.min(11);
        parts.push(prose(&SYN, seed * 101 + k, 1, n));
        left -= n;
        k += 1;
    }
    let t = parts.join(" ");
    assert_eq!(text::words(&t), words);
    json!({"id": id, "frozen_split": split, "origin": "human_only", "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "word_count": words,
        "collection": coll, "provenance": {"title": format!("synthetic {id}")}, "source_group": format!("g-{id}"), "domain": "web"})
}

fn s08(adds: &[(&str, u64, u64, u64, Value)]) -> Value {
    let mut doms = serde_json::Map::new();
    for (dm, h, l, f, floors) in adds {
        doms.insert(dm.to_string(), json!({"add": {"humans": h, "local_positives": l, "frontier_records": f, "positives": l + f, "local_for_generator_floor": floors}}));
    }
    json!({"id": "S0-8", "name": "supervision_targets", "value": {"schema": "slop_ninja_a_r2_domain_supervision_v1", "rule": "r2_domain_supervision_v1",
        "enron_domains": ["enron_email"], "rule3_generators": ["Family qwen36"], "domains": doms}})
}

fn key(x: &str, id: &str) -> String {
    text::sha_hex(format!("slop_ninja_a_comp_r2_plan_v1:{x}:{id}").as_bytes())
}

#[test]
fn topup_plan_fills_s0_8_from_train_mining_records_shortfalls_and_checks_the_label_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let f = topup_fixture(d);
    let target = s08(&[
        ("caselaw_access_project", 5, 4, 13, json!({"Family ernie_instruct": 2, "requested:gpt-6-astra": 1})),
        ("courtlistener", 50, 3, 0, json!({})),
        ("enron_email", 0, 2, 0, json!({})),
        ("wikipedia", 0, 6, 5, json!({})),
    ]);
    let vals = values_file(d, "values.json", json!({"S0-8": target.clone(), "S0-6": s06("0.8")}));
    let topup = |values: &Path, out: &Path, extra: &[&str]| {
        let mut a = vec!["topup-plan", "--values", s(values), "--train-mining", s(&f.tm), "--train-mining-manifest", s(&f.tmm), "--configs", s(&f.configs),
            "--frontier-config", s(&f.fc), "--train", s(&f.r0), "--train-manifest", s(&f.r0m), "--out", s(out)];
        a.extend_from_slice(extra);
        run(&a)
    };
    // S0-8 is the register's: refused while it is absent
    let no8 = values_file(d, "values-no-s08.json", json!({"S0-6": s06("0.8")}));
    refused(&topup(&no8, &d.join("t0"), &[]), "holds no S0-8");
    assert!(!d.join("t0").exists());
    // R0's Train and its manifest are required (the R0-id exclusion and the second parent source)
    let o = run(&["topup-plan", "--values", s(&vals), "--train-mining", s(&f.tm), "--train-mining-manifest", s(&f.tmm), "--configs", s(&f.configs),
        "--frontier-config", s(&f.fc), "--out", s(&d.join("t-no-r0"))]);
    refused(&o, "--train");
    assert!(!d.join("t-no-r0").exists());
    let out = d.join("topup");
    ok(&topup(&vals, &out, &[]));
    let t = read_json(&out.join("TOPUP.json"));
    let dm = &t["domains"];
    // rule 4: the first five in-window Train-mining humans of the domain by V21 key topup_human (150 and 450 words are inside)
    let cap_ids: Vec<String> = (0..12).map(|i| format!("cap-{i}")).chain(["cap-w150".to_string(), "cap-w450".to_string()]).collect();
    let mut cap: Vec<(String, String)> = cap_ids.iter().map(|id| (key("topup_human", id), id.clone())).collect();
    cap.sort();
    let want_cap: Vec<String> = cap[..5].iter().map(|x| x.1.clone()).collect();
    let humans = jsonl(&out.join("humans.jsonl"));
    let got: Vec<&str> = humans.iter().map(|h| h["id"].as_str().unwrap()).collect();
    let mut cl: Vec<(String, String)> = (0..3).map(|i| format!("cl-{i}")).map(|id| (key("topup_human", &id), id)).collect();
    cl.sort();
    let mut want_all: Vec<String> = want_cap.clone();
    want_all.extend(cl.iter().map(|x| x.1.clone()));
    assert_eq!(got.iter().copied().collect::<BTreeSet<_>>(), want_all.iter().map(String::as_str).collect::<BTreeSet<_>>());
    assert!(humans.iter().all(|h| h["frozen_split"] == "train" && h["split_assigned"] == true));
    // the excluded: outside the window (149 and 451 words among them), an Enron row, an Enron domain, no manifest domain, no planner domain, an R0 Train id
    let sup = &dm["caselaw_access_project"]["supply"]["train_mining"];
    assert_eq!((sup["candidate"].as_u64(), sup["below_window"].as_u64(), sup["above_window"].as_u64()), (Some(14), Some(3), Some(2)), "{sup}");
    assert_eq!((sup["enron_row (the Train half is D6's)"].as_u64(), sup["collection_without_a_planner_domain"].as_u64()), (Some(1), Some(1)), "{sup}");
    assert_eq!(dm["wikipedia"]["supply"]["train_mining"]["already_an_r0_train_human"], 1);
    assert_eq!(dm["wikipedia"]["supply"]["r0_train_parents_in_window"], 8);
    assert_eq!(t["train_mining_without_a_manifest_domain"], 1);
    assert_eq!(dm["enron_email"]["supply"]["train_mining"]["enron_domain"], 1);
    let sk = &t["lines_of_other_splits_skipped_unparsed"];
    assert_eq!(sk[format!("{}:humans", f.tm.display())], json!({"dev_select": 1, "test": 1}));
    assert_eq!(sk[format!("{}:manifest", f.tmm.display())], json!({"dev_select": 1}));
    // shortfalls by domain and rule; an unmapped label's rule-3 rows stay in the equal split, its cell unfilled
    let short = |t: &Value, d: &str| t["domains"][d]["shortfalls"].as_array().unwrap().iter().map(|x| x["rule"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(short(&t, "caselaw_access_project"), ["rule3_generator_floor"]);
    let un = &dm["caselaw_access_project"]["shortfalls"][0];
    assert_eq!((un["label"].as_str(), un["planned"].as_u64()), (Some("requested:gpt-6-astra"), Some(0)));
    assert!(un["rows_planned_as"].as_str().unwrap().contains("equal split"), "{un}");
    assert_eq!(dm["caselaw_access_project"]["local_mirrors"]["planned"], 4);
    assert_eq!(short(&t, "courtlistener"), ["rule4_humans"]);
    assert_eq!((dm["courtlistener"]["added_humans"].as_u64(), dm["courtlistener"]["shortfalls"][0]["wanted"].as_u64()), (Some(3), Some(50)));
    assert_eq!(short(&t, "enron_email"), ["rule1_local_positives"]);
    assert!(short(&t, "wikipedia").is_empty());
    // local mirrors: the added humans first, then R0's Train humans in the window by V21 key topup_parent
    let parents = jsonl(&out.join("parents.jsonl"));
    let pids: Vec<&str> = parents.iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(&pids[..4], &want_cap[..4].iter().map(String::as_str).collect::<Vec<_>>()[..]);
    assert_eq!(&pids[4..7], &cl.iter().map(|x| x.1.as_str()).collect::<Vec<_>>()[..]);
    let mut wp: Vec<(String, String)> = (0..8).map(|i| format!("wp-{i}")).map(|id| (key("topup_parent", &id), id)).collect();
    wp.sort();
    assert_eq!(&pids[7..], &wp[..6].iter().map(|x| x.1.as_str()).collect::<Vec<_>>()[..]);
    assert_eq!(t["outputs"]["parents"]["blocks"], json!([["caselaw_access_project", 4], ["courtlistener", 3], ["enron_email", 0], ["wikipedia", 6]]));
    // rule 3: a family's quota starts at its label's floor addition
    assert_eq!(t["family_quotas"]["caselaw_access_project"]["ernie_instruct"], 2);
    assert_eq!(t["family_quotas"]["caselaw_access_project"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), 4);
    // the frontier allocation (hand-computed) and its check; Enron holds none
    let a = read_json(&out.join("frontier").join("ALLOCATION.json"));
    assert_eq!(a["domains"]["caselaw_access_project"]["by_label"], json!({"gpt-5-6-terra": 2, "gpt-6-1-sol": 3, "gpt-6-1-terra": 3, "gpt-6-astra": 3, "gpt-6-sol": 2}));
    assert_eq!(a["domains"]["wikipedia"]["by_label"], json!({"gpt-5-6-terra": 0, "gpt-6-1-sol": 1, "gpt-6-1-terra": 2, "gpt-6-astra": 1, "gpt-6-sol": 1}));
    assert!(a["domains"]["enron_email"]["by_label"].as_object().unwrap().values().all(|v| v == 0));
    assert_eq!(a["frontier_config"]["sha256"], sha_file(&f.fc));
    assert_eq!(t["outputs"]["frontier_allocation"]["sha256"], sha_file(&out.join("frontier").join("ALLOCATION.json")));
    let c = ok(&run(&["topup-check", "--topup", s(&out), "--values", s(&vals)]));
    assert_eq!(c["passed"], true);
    // the check refuses a broken allocation, a changed or foreign ALLOCATION.json, and a plan of another S0-8
    let bent = d.join("topup-bent");
    std::fs::create_dir_all(bent.join("frontier")).unwrap();
    let write_bent = |a2: &Value, rerecord: bool| {
        std::fs::write(bent.join("frontier").join("ALLOCATION.json"), serde_json::to_vec(a2).unwrap()).unwrap();
        let mut t2 = t.clone();
        if rerecord {
            t2["outputs"]["frontier_allocation"]["sha256"] = json!(sha_file(&bent.join("frontier").join("ALLOCATION.json")));
        }
        std::fs::write(bent.join("TOPUP.json"), serde_json::to_vec(&t2).unwrap()).unwrap();
    };
    let mut a2 = a.clone();
    a2["domains"]["caselaw_access_project"]["by_label"]["gpt-6-1-terra"] = json!(2);
    a2["domains"]["caselaw_access_project"]["by_label"]["gpt-5-6-terra"] = json!(3);
    write_bent(&a2, true);
    refused(&run(&["topup-check", "--topup", s(&bent), "--values", s(&vals)]), "more to gpt-6.1-terra");
    a2["domains"]["caselaw_access_project"]["by_label"]["gpt-5-6-terra"] = json!(1);
    a2["domains"]["caselaw_access_project"]["by_label"]["gpt-6-1-terra"] = json!(2);
    a2["domains"]["caselaw_access_project"]["by_label"]["gpt-6-astra"] = json!(5);
    write_bent(&a2, true);
    refused(&run(&["topup-check", "--topup", s(&bent), "--values", s(&vals)]), "Terra >= Astra");
    // an ALLOCATION.json that passes the label rules but is not the file TOPUP.json records
    let mut a3 = a.clone();
    a3["passages"] = json!("changed");
    write_bent(&a3, false);
    refused(&run(&["topup-check", "--topup", s(&bent), "--values", s(&vals)]), "differs from the plan's record");
    // an allocation over another domain set than S0-8's, its digest re-recorded
    let mut a4 = a.clone();
    a4["domains"]["fineweb"] = a["domains"]["enron_email"].clone();
    write_bent(&a4, true);
    refused(&run(&["topup-check", "--topup", s(&bent), "--values", s(&vals)]), "the allocation's domains are not S0-8's");
    let other8 = s08(&[("caselaw_access_project", 5, 4, 14, json!({})), ("courtlistener", 50, 3, 0, json!({})), ("enron_email", 0, 2, 0, json!({})), ("wikipedia", 0, 6, 5, json!({}))]);
    let other = values_file(d, "values-other.json", json!({"S0-8": other8, "S0-6": s06("0.8")}));
    refused(&run(&["topup-check", "--topup", s(&out), "--values", s(&other)]), "another S0-8 value");
    // rule 2 is waived for Enron: an S0-8 that gives Enron frontier records is refused
    let enf = values_file(d, "values-enron-frontier.json", json!({"S0-8": s08(&[("caselaw_access_project", 1, 1, 1, json!({})), ("enron_email", 0, 0, 3, json!({}))])}));
    refused(&topup(&enf, &d.join("t-enf"), &[]), "rule 2 is waived");
    assert!(!d.join("t-enf").exists());
    let o = run(&["topup-plan", "--values", s(&vals), "--train-mining", s(&d.join("tm-dup.jsonl")), "--train-mining-manifest", s(&f.tmm), "--configs", s(&f.configs),
        "--frontier-config", s(&f.fc), "--train", s(&f.r0), "--train-manifest", s(&f.r0m), "--out", s(&d.join("t-dup"))]);
    refused(&o, "Train-mining line 2: a repeated human id");

    // supply ends: an R0 Train id is never added again (rule 4), R0's parents stay in the window, a mapped family's floor shortfall
    let target_b = s08(&[
        ("caselaw_access_project", 0, 0, 0, json!({})),
        ("courtlistener", 3, 10, 0, json!({"Family ernie_instruct": 8})),
        ("enron_email", 0, 0, 0, json!({})),
        ("wikipedia", 1, 9, 0, json!({})),
    ]);
    let vals_b = values_file(d, "values-b.json", json!({"S0-8": target_b, "S0-6": s06("0.8")}));
    let out_b = d.join("topup-b");
    ok(&topup(&vals_b, &out_b, &[]));
    let tb = read_json(&out_b.join("TOPUP.json"));
    let hb: Vec<String> = jsonl(&out_b.join("humans.jsonl")).iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
    assert!(!hb.contains(&"wp-0".to_string()), "{hb:?}");
    assert_eq!(hb.len(), 3);
    assert_eq!(short(&tb, "wikipedia"), ["rule4_humans", "rule1_local_positives"]);
    assert_eq!((tb["domains"]["wikipedia"]["added_humans"].as_u64(), tb["domains"]["wikipedia"]["local_mirrors"]["planned"].as_u64()), (Some(0), Some(8)));
    let pb: BTreeSet<String> = jsonl(&out_b.join("parents.jsonl")).iter().map(|h| h["id"].as_str().unwrap().to_string()).collect();
    assert!(!pb.contains("wp-short") && !pb.contains("wp-long") && pb.contains("wp-0"), "{pb:?}");
    assert_eq!(short(&tb, "courtlistener"), ["rule1_local_positives", "rule3_generator_floor"]);
    let fl = &tb["domains"]["courtlistener"]["shortfalls"][1];
    assert_eq!((fl["family"].as_str(), fl["wanted"].as_u64(), fl["planned"].as_u64()), (Some("ernie_instruct"), Some(8), Some(3)), "{fl}");

    // D5's local part: the Train-only mirror planner on the top-up's parents and family quotas
    let calsrc = cal_source(d, "r0-plan", true);
    let cf = s(&calsrc);
    let d5_run = |parents: &Path, topup: &Path, values: &Path, out: &Path| {
        run(&["train-mirror-plan", "--train", s(parents), "--topup", s(topup), "--values", s(values), "--cal-from", cf, "--out", s(out)])
    };
    let d5 = d.join("d5");
    let r = ok(&d5_run(&out.join("parents.jsonl"), &out, &vals, &d5));
    assert_eq!((r["items"].as_u64(), r["cohort"].as_str()), (Some(13), Some("train2_topup_mirror")));
    let items = jsonl(&d5.join("items.jsonl"));
    let cap_parents: BTreeSet<&str> = pids[..4].iter().copied().collect();
    let ernie = items.iter().filter(|i| cap_parents.contains(i["parent_id"].as_str().unwrap()) && i["generator"] == "ernie_instruct").count();
    assert_eq!(ernie, 2);
    let m = read_json(&d5.join("plan.json"));
    assert_eq!(m["train_mirror"]["source"]["topup_sha256"], sha_file(&out.join("TOPUP.json")));
    assert_eq!(m["train_mirror"]["family_quotas"]["caselaw_access_project"]["ernie_instruct"], 2);
    // the plan records the register's S0-8 (the top-up's) and the re-run check
    assert_eq!(m["train_mirror"]["s0_8"]["value"], t["s0_8"]["value"]);
    assert_eq!(m["train_mirror"]["s0_8"]["sha256"], sha_file(&vals));
    assert_eq!(m["train_mirror"]["source"]["topup_check"]["passed"], true);
    assert_decoding_ranks(&d5);
    // the quotas are the top-up's parents': another Train file is refused
    refused(&d5_run(&f.r0, &out, &vals, &d.join("d5-other")), "is not the top-up plan's parents.jsonl");
    // S0-8 must precede D5 in the register given: absent, or another value than the top-up's
    let only6 = values_file(d, "values-only-s06.json", json!({"S0-6": s06("0.8")}));
    refused(&d5_run(&out.join("parents.jsonl"), &out, &only6, &d.join("d5-no8")), "holds no S0-8");
    refused(&d5_run(&out.join("parents.jsonl"), &out, &other, &d.join("d5-other8")), "D5 follows the register's S0-8");
    // the parents file is read by its split bytes before any parse: an old Dev-select line refuses it even with its digest re-recorded
    let forged = d.join("topup-forged");
    std::fs::create_dir_all(forged.join("frontier")).unwrap();
    std::fs::copy(out.join("frontier").join("ALLOCATION.json"), forged.join("frontier").join("ALLOCATION.json")).unwrap();
    let pf = forged.join("parents.jsonl");
    std::fs::copy(out.join("parents.jsonl"), &pf).unwrap();
    append(&pf, b"{\"frozen_split\":\"dev_select\",\"id\":\"dev-sel-row\"}\n");
    let mut tf = t.clone();
    tf["outputs"]["parents"]["sha256"] = json!(sha_file(&pf));
    std::fs::write(forged.join("TOPUP.json"), serde_json::to_vec(&tf).unwrap()).unwrap();
    refused(&d5_run(&pf, &forged, &vals, &d.join("d5-forged")), "names a refused split (old Dev-select or the consumed Test) (dev_select)");
    // D5 re-runs the plan's label check: a broken allocation with its digest re-recorded is refused
    let bent5 = d.join("topup-bent5");
    std::fs::create_dir_all(bent5.join("frontier")).unwrap();
    std::fs::copy(out.join("parents.jsonl"), bent5.join("parents.jsonl")).unwrap();
    let mut a5 = a.clone();
    a5["domains"]["caselaw_access_project"]["by_label"]["gpt-6-1-terra"] = json!(2);
    a5["domains"]["caselaw_access_project"]["by_label"]["gpt-5-6-terra"] = json!(3);
    std::fs::write(bent5.join("frontier").join("ALLOCATION.json"), serde_json::to_vec(&a5).unwrap()).unwrap();
    let mut t5 = t.clone();
    t5["outputs"]["frontier_allocation"]["sha256"] = json!(sha_file(&bent5.join("frontier").join("ALLOCATION.json")));
    std::fs::write(bent5.join("TOPUP.json"), serde_json::to_vec(&t5).unwrap()).unwrap();
    refused(&d5_run(&bent5.join("parents.jsonl"), &bent5, &vals, &d.join("d5-bent")), "more to gpt-6.1-terra");
    for p in ["d5-other", "d5-no8", "d5-other8", "d5-forged", "d5-bent"] {
        assert!(!d.join(p).exists(), "{p}");
    }
}

fn edit(rid: &str, g: &str, k: usize) -> Edit {
    Edit {
        edit_index: k,
        component: "run".into(),
        kind: "run".into(),
        operation: "rewrite".into(),
        family: "f".into(),
        operation_requested: "rewrite".into(),
        generator: g.into(),
        parent_span: (k * 10, k * 10 + 5),
        insert_sep: None,
        sentence_range: (k, k + 1),
        context_included: false,
        context_spans: vec![],
        style: json!({}),
        template_id: splice::T_RUN.into(),
        source_words: 5,
        target_words: 5,
        len_lo: 3.0,
        len_hi: 8.0,
        min_change: None,
        start_rule: "s".into(),
        end_rule: "e".into(),
        newline_ok: false,
        request_id: rid.into(),
    }
}

/// A synthetic spliced plan: `train` items in two domains, an item of every
/// fifth with edits of both generators, and a few Dev-iter items.
fn spliced_plan(pdir: &Path, wiki: usize, news: usize) -> Vec<SItem> {
    std::fs::create_dir_all(pdir.join("requests")).unwrap();
    let mut items = Vec::new();
    let mut reqs: BTreeMap<&str, Vec<plan::Request>> = BTreeMap::new();
    let n = wiki + news + 3;
    for i in 0..n {
        let (pop, split, dom) = if i >= wiki + news { ("dev_iter_span_eval", "dev_iter", "wiki") } else if i < wiki { ("train", "train", "wiki") } else { ("train", "train", "news") };
        let id = format!("sp-{i:03}");
        let gens: Vec<&str> = if i % 5 == 0 { vec!["g1", "g2"] } else if i % 2 == 0 { vec!["g1"] } else { vec!["g2"] };
        let edits: Vec<Edit> = gens.iter().enumerate().map(|(k, g)| edit(&format!("rq-{i:03}-{k}"), g, k)).collect();
        for e in &edits {
            reqs.entry(if e.generator == "g1" { "g1" } else { "g2" }).or_default().push(plan::Request {
                request_id: e.request_id.clone(), item_id: id.clone(), parent_id: format!("p-{i}"), split: split.into(), generator: e.generator.clone(),
                endpoint: "chat".into(), template_id: e.template_id.clone(), prompt: "p".into(), span_index: e.edit_index, source_span: Some(e.parent_span),
                target_words: 5, cut_budget: None, decoding: plan::profile("A", 0.7, 0.9, 64), request_sha256: "0".repeat(64), seed: 1, op: None, cohort: None, attempt_limit: None,
            });
        }
        items.push(SItem {
            item_id: id, population: pop.into(), split: split.into(), domain: dom.into(), parent_id: format!("p-{i}"), parent_text_sha256: "0".repeat(64),
            source_group: format!("g-{i}"), writer_key: None, variant: 0, order: n - i, order_t: 0.5, policy: "run".into(), policy_requested: "run".into(),
            policy_fallback_steps: 0, generators: gens.iter().map(|g| g.to_string()).collect(), two_generators: gens.len() == 2, voice_scope: "document".into(),
            span_half: None, edits,
        });
    }
    let mut b = Vec::new();
    for it in &items {
        serde_json::to_writer(&mut b, it).unwrap();
        b.push(b'\n');
    }
    std::fs::write(pdir.join("items.jsonl"), &b).unwrap();
    std::fs::write(pdir.join("parents.jsonl"), b"").unwrap();
    let mut rf = serde_json::Map::new();
    for (g, v) in &reqs {
        let mut rb = Vec::new();
        for r in v {
            serde_json::to_writer(&mut rb, r).unwrap();
            rb.push(b'\n');
        }
        std::fs::write(pdir.join("requests").join(format!("{g}.jsonl")), &rb).unwrap();
        rf.insert(g.to_string(), json!({"requests": v.len(), "sha256": text::sha_hex(&rb)}));
    }
    std::fs::write(pdir.join("plan.json"), serde_json::to_vec(&json!({"schema": splice::PLAN_SCHEMA, "items": {"sha256": text::sha_hex(&b)},
        "parents": {"sha256": text::sha_hex(b"")}, "request_files": rf})).unwrap()).unwrap();
    items
}

/// A synthetic renorm spliced output for population `train`: one row per
/// edit of the items `regen` (regenerate) and `drop` (drop_out_of_attempts).
fn spliced_renorm(out: &Path, pdir: &Path, items: &[SItem], regen: &BTreeSet<String>, drop: &BTreeSet<String>) {
    std::fs::create_dir_all(out).unwrap();
    let mut b = Vec::new();
    for it in items {
        let a = if regen.contains(&it.item_id) { "regenerate" } else if drop.contains(&it.item_id) { "drop_out_of_attempts" } else { continue };
        for e in &it.edits {
            serde_json::to_writer(&mut b, &json!({"schema": "slop_ninja_a_positives_regeneration_plan_v1", "request_id": e.request_id, "item_id": it.item_id,
                "generator": e.generator, "batch": "population-train", "action": a})).unwrap();
            b.push(b'\n');
        }
    }
    let rg = "regeneration-plan-population-train.jsonl";
    std::fs::write(out.join(rg), &b).unwrap();
    std::fs::write(out.join("RENORM-population-train.json"), serde_json::to_vec(&json!({"schema": "slop_ninja_a_positives_renorm_v1", "plan_kind": "spliced",
        "batch": "population-train", "inputs": {"plan_sha256": sha_file(&pdir.join("plan.json")), "items_sha256": sha_file(&pdir.join("items.jsonl"))},
        "outputs": {"regeneration_plan": {"path": rg, "sha256": text::sha_hex(&b)}}})).unwrap()).unwrap();
}

/// The pilot rule spelled out independently of `pilot`: rows in ascending
/// V21 key x = pilot; the whole step when a cell holds fewer than 59 rows,
/// else the shortest prefix holding 59 of every cell.
fn brute_pilot(rows: &[(String, String)]) -> BTreeSet<String> {
    let mut keyed: Vec<(String, &(String, String))> = rows.iter().map(|r| (key("pilot", &r.0), r)).collect();
    keyed.sort();
    let cells: BTreeSet<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    if cells.iter().any(|c| rows.iter().filter(|r| r.1 == *c).count() < 59) {
        return rows.iter().map(|r| r.0.clone()).collect();
    }
    for k in 0..=keyed.len() {
        if cells.iter().all(|c| keyed[..k].iter().filter(|(_, r)| r.1 == *c).count() >= 59) {
            return keyed[..k].iter().map(|(_, r)| r.0.clone()).collect();
        }
    }
    unreachable!()
}

#[test]
fn d8_pilot_takes_renorms_regenerate_items_of_a_spliced_plan_and_the_spliced_generator_runs_only_them() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let pdir = d.join("spliced");
    let items = spliced_plan(&pdir, 130, 80);
    let train: Vec<&SItem> = items.iter().filter(|i| i.population == "train").collect();
    // regenerate: every train item but the drops; drop_out_of_attempts: every seventh
    let drop: BTreeSet<String> = train.iter().filter(|i| i.item_id.ends_with('7')).map(|i| i.item_id.clone()).collect();
    let regen: BTreeSet<String> = train.iter().filter(|i| !drop.contains(&i.item_id)).map(|i| i.item_id.clone()).collect();
    let rn = d.join("renorm");
    spliced_renorm(&rn, &pdir, &items, &regen, &drop);
    let pp = d.join("pilot");
    let r = ok(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn), "--step", "D8", "--out", s(&pp)]));
    let pj = read_json(&pp.join("PILOT.json"));
    assert_eq!(pj["rows"].as_u64(), Some(regen.len() as u64));
    assert_eq!(pj["renorm"]["actions"], json!({"drop_out_of_attempts": drop.iter().map(|id| items.iter().find(|i| &i.item_id == id).unwrap().edits.len()).sum::<usize>(),
        "regenerate": regen.iter().map(|id| items.iter().find(|i| &i.item_id == id).unwrap().edits.len()).sum::<usize>()}));
    let rows: Vec<(String, String)> = train.iter().filter(|i| regen.contains(&i.item_id)).map(|i| (i.item_id.clone(), format!("{}|mixed", i.domain))).collect();
    let want = brute_pilot(&rows);
    assert!(want.len() < rows.len());
    assert_eq!((r["pilot_rows"].as_u64(), pj["whole_step_is_pilot"].as_bool()), (Some(want.len() as u64), Some(false)));
    let pc: Vec<u64> = ["news|mixed", "wiki|mixed"].iter().map(|c| pj["cells"][c]["pilot"].as_u64().unwrap()).collect();
    assert!(pc.iter().all(|n| *n >= 59) && pc.contains(&59), "{pc:?}");
    // the order files: per generator, every pilot (bulk) item with an edit of it, in plan order
    for g in ["g1", "g2"] {
        let mut its: Vec<&&SItem> = train.iter().filter(|i| regen.contains(&i.item_id) && i.edits.iter().any(|e| e.generator == g)).collect();
        its.sort_by_key(|i| i.order);
        for (name, inside) in [("pilot", true), ("bulk", false)] {
            let w: String = its.iter().filter(|i| want.contains(&i.item_id) == inside).map(|i| format!("{}\n", i.item_id)).collect();
            assert_eq!(std::fs::read_to_string(pp.join(format!("{name}-order-{g}.txt"))).unwrap_or_default(), w, "{name} {g}");
        }
    }
    // a cell below the floor pilots D8 whole
    let regen_small: BTreeSet<String> = regen.iter().filter(|id| train.iter().find(|i| &&i.item_id == id).unwrap().domain == "wiki" || id.ends_with('1')).cloned().collect();
    let rn2 = d.join("renorm-small");
    spliced_renorm(&rn2, &pdir, &items, &regen_small, &BTreeSet::new());
    let p2 = ok(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn2), "--step", "D8", "--out", s(&d.join("pilot-small"))]));
    assert_eq!(p2["pilot_rows"], p2["rows"]);
    assert_eq!(read_json(&d.join("pilot-small").join("PILOT.json"))["cells_below_floor"], json!(["news|mixed"]));
    // the spliced generator runs only the listed items, in list order, with the population's batch
    let cfg = d.join("g1.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": "g1", "allowed_splits": ["train"]})).unwrap()).unwrap();
    let runs = d.join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let gen_run = |order: &Path, extra: &[&str]| {
        Command::new(SPLICED)
            .args(["generate", "--plan", s(&pdir), "--population", "train", "--generator", "g1", "--config", s(&cfg), "--runs", s(&runs), "--index", s(&d.join("no-index"))])
            .args(["--dry-run", "--request-order", s(order)])
            .args(extra)
            .output()
            .unwrap()
    };
    let v = ok(&gen_run(&pp.join("pilot-order-{GEN}.txt"), &[]));
    let listed: Vec<String> = std::fs::read_to_string(pp.join("pilot-order-g1.txt")).unwrap().lines().map(String::from).collect();
    assert_eq!(v["item_order"], json!(listed));
    assert_eq!(v["pending_items"], json!(listed.iter().collect::<BTreeSet<_>>()));
    assert_eq!(v["request_order"]["sha256"], sha_file(&pp.join("pilot-order-g1.txt")));
    // list order, not plan order: the same items listed in reverse run in reverse
    let rev = d.join("order-g1-reversed.txt");
    std::fs::write(&rev, listed.iter().rev().map(|i| format!("{i}\n")).collect::<String>()).unwrap();
    let v = ok(&gen_run(&rev, &[]));
    assert_eq!(v["item_order"], json!(listed.iter().rev().collect::<Vec<_>>()));
    // refusals: an order window beside the order file; an item of another population; an item without an edit of the generator
    refused(&gen_run(&pp.join("pilot-order-{GEN}.txt"), &["--from", "3"]), "replaces the order window");
    let dev = d.join("order-dev.txt");
    std::fs::write(&dev, format!("{}\n", items.iter().find(|i| i.population != "train" && i.edits.iter().any(|e| e.generator == "g1")).unwrap().item_id)).unwrap();
    refused(&gen_run(&dev, &[]), "is not a train item with an edit of g1");
    let g2only = d.join("order-g2.txt");
    std::fs::write(&g2only, format!("{}\n", train.iter().find(|i| i.edits.iter().all(|e| e.generator == "g2")).unwrap().item_id)).unwrap();
    refused(&gen_run(&g2only, &[]), "with an edit of g1");
    // the pilot's bindings: renorm of another plan, a changed regeneration plan, another population's item; the flags
    let rn3 = d.join("renorm-other-plan");
    spliced_renorm(&rn3, &pdir, &items, &regen, &drop);
    let mut sum = read_json(&rn3.join("RENORM-population-train.json"));
    sum["inputs"]["plan_sha256"] = json!("0".repeat(64));
    std::fs::write(rn3.join("RENORM-population-train.json"), serde_json::to_vec(&sum).unwrap()).unwrap();
    refused(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn3), "--step", "D8", "--out", s(&d.join("px1"))]), "re-verdicted another plan");
    let rn4 = d.join("renorm-changed");
    spliced_renorm(&rn4, &pdir, &items, &regen, &drop);
    append(&rn4.join("regeneration-plan-population-train.jsonl"), b"\n");
    refused(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn4), "--step", "D8", "--out", s(&d.join("px2"))]), "differs from renorm's summary");
    let rn5 = d.join("renorm-dev-item");
    let devs: BTreeSet<String> = items.iter().filter(|i| i.population != "train").map(|i| i.item_id.clone()).collect();
    spliced_renorm(&rn5, &pdir, &items, &devs, &BTreeSet::new());
    refused(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn5), "--step", "D8", "--out", s(&d.join("px3"))]), "is not a train item of the plan");
    // a regeneration row whose request is another item's edit, or whose generator is not its edit's
    let tamper_row = |name: &str, f: &dyn Fn(&mut Value)| -> PathBuf {
        let rn = d.join(name);
        spliced_renorm(&rn, &pdir, &items, &regen, &drop);
        let rgp = rn.join("regeneration-plan-population-train.jsonl");
        let mut rows = jsonl(&rgp);
        f(&mut rows[0]);
        write_rows(&rgp, &rows);
        let mut sum = read_json(&rn.join("RENORM-population-train.json"));
        sum["outputs"]["regeneration_plan"]["sha256"] = json!(sha_file(&rgp));
        std::fs::write(rn.join("RENORM-population-train.json"), serde_json::to_vec(&sum).unwrap()).unwrap();
        rn
    };
    let rn6 = tamper_row("renorm-foreign-request", &|r| {
        let mine = r["item_id"].as_str().unwrap().to_string();
        let other = train.iter().find(|i| i.item_id != mine).unwrap().edits[0].request_id.clone();
        r["request_id"] = json!(other);
    });
    refused(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn6), "--step", "D8", "--out", s(&d.join("px6"))]), "not an edit of item");
    let rn7 = tamper_row("renorm-foreign-generator", &|r| {
        let g = if r["generator"] == "g1" { "g9" } else { "g1x" };
        r["generator"] = json!(g);
    });
    refused(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn7), "--step", "D8", "--out", s(&d.join("px7"))]), "not an edit of item");
    // a plan population whose items are of two splits
    let pmix = d.join("spliced-mixed");
    let mut mixed = spliced_plan(&pmix, 130, 80);
    mixed[1].split = "dev_iter".into();
    let mut b = Vec::new();
    for it in &mixed {
        serde_json::to_writer(&mut b, it).unwrap();
        b.push(b'\n');
    }
    std::fs::write(pmix.join("items.jsonl"), &b).unwrap();
    let mut pm = read_json(&pmix.join("plan.json"));
    pm["items"]["sha256"] = json!(text::sha_hex(&b));
    std::fs::write(pmix.join("plan.json"), serde_json::to_vec(&pm).unwrap()).unwrap();
    let rn8 = d.join("renorm-mixed");
    spliced_renorm(&rn8, &pmix, &mixed, &regen, &drop);
    refused(&run(&["pilot", "--plan", s(&pmix), "--population", "train", "--renorm", s(&rn8), "--step", "D8", "--out", s(&d.join("px8"))]), "its items are not of one split");
    refused(&run(&["pilot", "--plan", s(&pdir), "--step", "D8", "--out", s(&d.join("px4"))]), "takes --population and --renorm");
    refused(&run(&["pilot", "--plan", s(&pdir), "--population", "train", "--renorm", s(&rn), "--request-order", "x-{GEN}", "--step", "D8", "--out", s(&d.join("px5"))]), "not --request-order");
    for p in ["px1", "px2", "px3", "px4", "px5", "px6", "px7", "px8"] {
        assert!(!d.join(p).exists(), "{p}");
    }
}

#[test]
fn s0_10_is_written_from_the_base_selection_and_d7_follows_it() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let tr = d.join("train.jsonl");
    let mut rows: Vec<Value> = (0..14).map(|i| human(&format!("fw-{i}"), "train", "fineweb_cc_historical", i, 15)).collect();
    rows.extend((0..10).map(|i| human(&format!("wp-{i}"), "train", "wikipedia_enwiki_historical", 100 + i, 15)));
    write_rows(&tr, &rows);
    let pdir = d.join("base");
    ok(&run(&["plan-v3", "--train", s(&tr), "--base-generators", "granite_base,smollm3_base,ernie_base", "--out", s(&pdir)]));
    let items = jsonl(&pdir.join("items.jsonl"));
    let runs = d.join("runs");
    std::fs::create_dir_all(runs.join("granite_base")).unwrap();
    let accept = |ids: &[String], file: &str| {
        let mut j = String::new();
        for id in ids {
            let it = items.iter().find(|v| v["item_id"] == id.as_str()).unwrap();
            let rid = it["requests"][0].as_str().unwrap();
            j.push_str(&format!("{}\n{}\n", json!({"kind":"intent","request_id":rid,"split":"train","attempt":0}),
                json!({"kind":"response","request_id":rid,"split":"train","attempt":0,"verdict":{"accepted":true,"text":"Synthetic words."}})));
        }
        std::fs::write(runs.join("granite_base").join(file), j).unwrap();
    };
    let first = items.iter().find(|v| v["generator"] == "granite_base").unwrap()["item_id"].as_str().unwrap().to_string();
    accept(std::slice::from_ref(&first), "journal-1-w0.jsonl");
    let parts = d.join("parts.jsonl");
    std::fs::write(&parts, "").unwrap();
    let v7 = json!({"id": "S0-7", "name": "v_fam", "value": 6});
    let vals = values_file(d, "values.json", json!({"S0-7": v7}));
    const SEED: &str = "r2_base_cohort_stratification_v1";
    let base_args = |values: &Path, out: &Path| -> Vec<String> {
        ["stratify-train", "--plan", s(&pdir), "--runs", s(&runs), "--partitions", s(&parts), "--cohort-shares", "train_base_v3=1",
            "--target", "granite_base", "--target", "smollm3_base", "--target", "ernie_base", "--values", s(values), "--seed", SEED, "--out", s(out)]
            .iter()
            .map(|x| x.to_string())
            .collect()
    };
    let st = |values: &Path, out: &Path, extra: &[&str]| {
        let mut a = base_args(values, out);
        a.extend(extra.iter().map(|x| x.to_string()));
        Command::new(BIN).args(&a).output().unwrap()
    };
    // S0-10's entry, from part 1's selection at v_fam, by Train human counts (floor 0)
    let entry = d.join("s0-10.json");
    ok(&st(&vals, &d.join("sel-w"), &["--write-stratification", s(&entry)]));
    assert!(!d.join("sel-w").exists());
    let e = read_json(&entry);
    assert_eq!((e["id"].as_str(), e["name"].as_str(), e["value"]["schema"].as_str()), (Some("S0-10"), Some("base_cohort_stratification"), Some("slop_ninja_a_positives_base_stratification_v1")));
    for g in ["granite_base", "smollm3_base", "ernie_base"] {
        assert_eq!(e["value"]["targets"][g].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), 6, "{g}");
    }
    assert_eq!(e["value"]["existing_accepted_train"]["granite_base"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), 1);
    assert_eq!((e["value"]["v_fam"].as_u64(), e["value"]["floor"].as_u64(), e["value"]["seed"].as_str()), (Some(6), Some(0), Some(SEED)));
    assert_eq!(e["value"]["domain_weights"], json!({}));
    // refusals: written once; S0-10's fixed options (amendment 27); every base family once; not both modes
    let w = |extra: &[&str], name: &str, want: &str| {
        let p = d.join(format!("e-{name}.json"));
        let mut a: Vec<&str> = vec!["--write-stratification", s(&p)];
        a.extend_from_slice(extra);
        refused(&st(&vals, &d.join(format!("sel-{name}")), &a), want);
        assert!(!p.exists(), "{name}");
    };
    refused(&st(&vals, &d.join("sel-w2"), &["--write-stratification", s(&entry)]), "written once");
    w(&["--generator", "ernie_base=2"], "budget", "--target generators only");
    w(&["--stratification"], "both", "not both");
    w(&["--floor", "1"], "floor", "S0-10's floor is 0");
    w(&["--domain-weights", "fineweb=2"], "weights", "--domain-weights is refused");
    w(&["--priority-domains", "fineweb"], "prio", "--priority-domains is refused");
    let two = d.join("e-two.json");
    let mut a = base_args(&vals, &d.join("sel-two"));
    let at = a.iter().position(|x| x == "ernie_base").unwrap();
    a.drain(at - 1..=at);
    a.extend(["--write-stratification".to_string(), s(&two).to_string()]);
    refused(&Command::new(BIN).args(&a).output().unwrap(), "every Train base family once");
    for (flag, val, want) in [("--cohort-shares", "train_base_v3=0.5", "cohort shares are train_base_v3=1"), ("--seed", "other-seed", "seed is r2_base_cohort_stratification_v1")] {
        let mut a = base_args(&vals, &d.join("sel-opt"));
        let at = a.iter().position(|x| x == flag).unwrap();
        a[at + 1] = val.to_string();
        a.extend(["--write-stratification".to_string(), s(&d.join("e-opt.json")).to_string()]);
        refused(&Command::new(BIN).args(&a).output().unwrap(), want);
    }
    assert!(!two.exists() && !d.join("e-opt.json").exists());
    // D7's step is refused while S0-10 is absent
    refused(&st(&vals, &d.join("sel-d7-0"), &["--stratification"]), "holds no S0-10");
    assert!(!d.join("sel-d7-0").exists());
    let vals10 = values_file(d, "values-10.json", json!({"S0-7": v7, "S0-10": e}));
    // a first pass equals part 1's selection at the same floor and seed
    let p1 = d.join("sel-p1");
    ok(&st(&vals, &p1, &["--floor", "0"]));
    let d7 = d.join("sel-d7");
    ok(&st(&vals10, &d7, &["--stratification"]));
    for g in ["granite_base", "smollm3_base", "ernie_base"] {
        assert_eq!(std::fs::read(p1.join(format!("{g}.txt"))).unwrap(), std::fs::read(d7.join(format!("{g}.txt"))).unwrap(), "{g}");
    }
    let sel = read_json(&d7.join("selection.json"));
    assert_eq!(sel["stratification"]["record"]["sha256"], sha_file(&vals10));
    assert!(read_json(&p1.join("selection.json")).get("stratification").is_none());
    // D7 takes S0-10's fixed options too
    refused(&st(&vals10, &d.join("sel-d7-floor"), &["--stratification", "--floor", "1"]), "S0-10's floor is 0");
    refused(&st(&vals10, &d.join("sel-d7-w"), &["--stratification", "--domain-weights", "fineweb=2"]), "--domain-weights is refused");
    // D7 follows S0-10's inputs: an entry of another plan, v_fam, prior plans, partitions, floor, seed, cohort shares, domain weights or human counts is refused
    let tampered: [(&str, Value); 9] = [
        ("plan", json!({"plan_sha256": "0".repeat(64), "items_sha256": e["value"]["plan"]["items_sha256"]})),
        ("v_fam", json!(7)),
        ("prior_plans", json!([{"plan": "/elsewhere", "plan_sha256": "1".repeat(64)}])),
        ("partitions_sha256", json!("2".repeat(64))),
        ("floor", json!(1)),
        ("seed", json!("other-seed")),
        ("cohort_shares", json!([["train_base_v3", 0.5]])),
        ("domain_weights", json!({"fineweb": 2.0})),
        ("train_humans", json!({"fineweb": 99.0, "wikipedia": 10.0})),
    ];
    for (k, v) in tampered {
        let mut e2 = e.clone();
        assert_ne!(e2["value"][k], v, "{k}");
        e2["value"][k] = v;
        let vt = values_file(d, &format!("values-10-{k}.json"), json!({"S0-7": v7, "S0-10": e2}));
        refused(&st(&vt, &d.join(format!("sel-t-{k}")), &["--stratification"]), &format!("S0-10's {k} differs"));
        assert!(!d.join(format!("sel-t-{k}")).exists(), "{k}");
    }
    // a register whose S0-7 is another v_fam than S0-10's
    let v7b = values_file(d, "values-10-v7.json", json!({"S0-7": {"id": "S0-7", "name": "v_fam", "value": 7}, "S0-10": e}));
    refused(&st(&v7b, &d.join("sel-v7"), &["--stratification"]), "S0-10's v_fam differs");
    // a later pass fills the frozen targets: the first pass's granite items accepted, the targets less them remain
    let picked: Vec<String> = std::fs::read_to_string(d7.join("granite_base.txt")).unwrap().lines().filter(|l| !l.starts_with('#')).map(String::from).collect();
    accept(&picked[..2], "journal-2-w0.jsonl");
    let d7b = d.join("sel-d7-b");
    ok(&st(&vals10, &d7b, &["--stratification"]));
    let sel = read_json(&d7b.join("selection.json"));
    let tg = &e["value"]["targets"]["granite_base"];
    let q = &sel["stratification"]["quota_this_pass"]["granite_base"];
    let mut acc: BTreeMap<String, u64> = BTreeMap::new();
    for id in std::iter::once(&first).chain(&picked[..2]) {
        *acc.entry(items.iter().find(|v| v["item_id"] == id.as_str()).unwrap()["domain"].as_str().unwrap().to_string()).or_default() += 1;
    }
    for (dm, t) in tg.as_object().unwrap() {
        assert_eq!(q[dm].as_u64(), Some(t.as_u64().unwrap().saturating_sub(acc.get(dm).copied().unwrap_or(0))), "{dm}");
    }
    assert_eq!(sel["generators"]["granite_base"]["budget"].as_u64(), Some(6 - 3));
}

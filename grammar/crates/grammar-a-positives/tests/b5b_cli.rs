//! B5b (BUILD-PLAN section 4 and section 10.4; rulings R2 and R6) run as the
//! binary on synthetic text (no corpus text, no model or network call
//! beyond a loopback stand-in server):
//! - `split-copy` selects lines by the split read from their bytes and
//!   copies them byte for byte; `generate` and `finalize` refuse a file of
//!   more than one split before any parse; a one-split copy runs;
//! - D4's plan (`mirror-plan`): renorm's `regenerate_mirror` rows as new
//!   requests at `mirror_target_words_v2` with their remaining attempts,
//!   judged in renorm's batch on renorm's output runs; attempts stop at the
//!   carried limit; the keyed pilot (`pilot`) of the D-step;
//! - `stratify-train` parses only Train lines, and `--target` budgets v_fam
//!   read from VALUES-R2.json (S0-7);
//! - `enron-half` writes the V23 Train half from the bound pool, checked
//!   against S0-9;
//! - the B5b review's findings 3-9: files of a refused split alone, the
//!   split checks of `generate --profile` and `finalize`, mirror-plan's
//!   renorm bindings, carried limits in renorm and `rates`, the pilot's
//!   order files and the CAL overlap extra (amendment 24).
use grammar_a_positives::{generation, plan, text};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_a_positives");

const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
    "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
];
const ALT: [&str; 20] = [
    "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
    "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
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

/// Synthetic humans of one split in two evaluation domains (about 121 words each).
fn write_humans(path: &Path, split: &str, per_domain: u64) {
    let mut out = String::new();
    for (d, coll) in [("fw", "fineweb_cc_historical"), ("wp", "wikipedia_enwiki_historical")] {
        for k in 0..per_domain {
            let t = format!("{}\n\n{}", prose(&SYN, 100 + k, 5, 11), prose(&SYN, 200 + k, 6, 11));
            let row = json!({"id":format!("{split}-{d}-{k}"),"frozen_split":split,"origin":"human_only","text":t,
                "text_sha256":text::sha_hex(t.as_bytes()),"word_count":text::words(&t),"collection":coll,
                "provenance":{"title":format!("synthetic {k}")},"source_group":format!("g-{split}-{d}-{k}"),"domain":"web"});
            out.push_str(&serde_json::to_string(&row).unwrap());
            out.push('\n');
        }
    }
    std::fs::write(path, out).unwrap();
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

fn jsonl(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn sha_file(p: &Path) -> String {
    text::sha_hex(&std::fs::read(p).unwrap())
}

/// The lines of `p` whose parsed `split` is `split`, with their newlines.
fn lines_of(p: &Path, split: &str) -> Vec<u8> {
    let mut o = Vec::new();
    for l in std::fs::read_to_string(p).unwrap().lines() {
        if !l.trim().is_empty() && serde_json::from_str::<Value>(l).map(|v| v["split"] == split).unwrap_or(false) {
            o.extend_from_slice(l.as_bytes());
            o.push(b'\n');
        }
    }
    o
}

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
    std::fs::OpenOptions::new().append(true).open(p).unwrap().write_all(b).unwrap();
}

/// Re-records the digests of a plan's files in its plan.json.
fn rehash(pdir: &Path) {
    let mut m = read_json(&pdir.join("plan.json"));
    m["items_sha256"] = json!(sha_file(&pdir.join("items.jsonl")));
    m["plan_failures_sha256"] = json!(sha_file(&pdir.join("plan-failures.jsonl")));
    let gens: Vec<String> = m["request_files"].as_object().unwrap().keys().cloned().collect();
    for g in gens {
        m["request_files"][&g]["sha256"] = json!(sha_file(&pdir.join("requests").join(format!("{g}.jsonl"))));
    }
    std::fs::write(pdir.join("plan.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
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

/// A synthetic VALUES-R2.json with the given `values` entries.
fn values_file(d: &Path, name: &str, entries: Value) -> PathBuf {
    let p = d.join(name);
    std::fs::write(&p, serde_json::to_vec(&json!({"schema": "slop_ninja_a_round2_values_v1", "values": entries})).unwrap()).unwrap();
    p
}

/// A file of `split`'s lines only, each with an unparsable tail (a reader
/// that parses one fails loudly).
fn refused_only(split: &str) -> Vec<u8> {
    format!("{{\"split\":\"{split}\",\"item_id\":\"x\",\"request_id\":\"x\",\x00 never parsed\n{{\"split\" : \"{split}\", broken\n").into_bytes()
}

/// A junk line whose split key is readable and whose rest is not JSON: any
/// parse of it fails, so a reader that parses it fails loudly.
const JUNK: &[u8] = b"{\"split\":\"dev_select\",\"item_id\":\"x\",\x00 never parsed\n{\"id\":\"t\", \"split\" : \"test\", broken\n";

#[test]
fn split_copy_selects_lines_by_their_split_bytes_and_generate_refuses_two_splits() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (tr, di) = (d.join("train.jsonl"), d.join("dev-iter.jsonl"));
    write_humans(&tr, "train", 8);
    write_humans(&di, "dev_iter", 8);
    let pdir = d.join("plan");
    ok(&run(&["plan-v3", "--train", s(&tr), "--dev-iter", s(&di), "--splits", "train,dev_iter", "--out", s(&pdir)]));
    // the generator whose request file holds both splits
    let meta0 = read_json(&pdir.join("plan.json"));
    let gens: Vec<String> = meta0["request_files"].as_object().unwrap().keys().cloned().collect();
    let g = gens
        .iter()
        .find(|g| {
            let v = jsonl(&pdir.join("requests").join(format!("{g}.jsonl")));
            v.iter().any(|r| r["split"] == "train") && v.iter().any(|r| r["split"] == "dev_iter")
        })
        .unwrap()
        .clone();
    // the source holds old Dev-select and consumed-Test lines that no reader may parse (plans v1 and v2 do)
    let src = d.join("plan-mixed");
    copy_dir(&pdir, &src);
    append(&src.join("items.jsonl"), JUNK);
    append(&src.join("plan-failures.jsonl"), JUNK);
    append(&src.join("requests").join(format!("{g}.jsonl")), JUNK);
    rehash(&src);
    let copy = d.join("copy-train");
    let o = ok(&run(&["split-copy", "--plan", s(&src), "--split", "train", "--out", s(&copy)]));
    let m = read_json(&copy.join("plan.json"));
    let rec = &m["split_copy"];
    // selected lines are byte for byte the source's train lines
    assert_eq!(std::fs::read(copy.join("items.jsonl")).unwrap(), lines_of(&pdir.join("items.jsonl"), "train"));
    assert_eq!(std::fs::read(copy.join("plan-failures.jsonl")).unwrap(), lines_of(&pdir.join("plan-failures.jsonl"), "train"));
    let mut n_req = 0;
    for g in &gens {
        let want = lines_of(&pdir.join("requests").join(format!("{g}.jsonl")), "train");
        let p = copy.join("requests").join(format!("{g}.jsonl"));
        if want.is_empty() {
            assert!(!p.exists() && m["request_files"].get(g).is_none(), "{g}");
        } else {
            assert_eq!(std::fs::read(&p).unwrap(), want, "{g}");
            assert_eq!(m["request_files"][g]["sha256"], sha_file(&p));
            n_req += String::from_utf8(want).unwrap().lines().count();
        }
    }
    // counts by split: the dev_iter lines and the junk lines, skipped unparsed
    let sk = &rec["lines_of_other_splits_skipped_unparsed"];
    for k in ["items", "requests", "plan_failures"] {
        assert_eq!((sk[k]["dev_select"].as_u64(), sk[k]["test"].as_u64()), (Some(1), Some(1)), "{k}: {sk}");
    }
    let dev_fails = jsonl(&pdir.join("plan-failures.jsonl")).iter().filter(|v| v["split"] == "dev_iter").count();
    assert_eq!(sk["plan_failures"]["dev_iter"].as_u64().unwrap_or(0), dev_fails as u64);
    let dev_items = jsonl(&pdir.join("items.jsonl")).iter().filter(|v| v["split"] == "dev_iter").count();
    assert_eq!(sk["items"]["dev_iter"].as_u64(), Some(dev_items as u64));
    assert_eq!(o["copied_lines"]["requests"].as_u64(), Some(n_req as u64));
    // the copy's plan.json: its files' digests, the split's input only, the source's digests
    assert_eq!(m["items_sha256"], sha_file(&copy.join("items.jsonl")));
    assert_eq!(m["plan_failures_sha256"], sha_file(&copy.join("plan-failures.jsonl")));
    assert_eq!(m["inputs"].as_object().unwrap().keys().collect::<Vec<_>>(), ["train"]);
    assert_eq!(m["inputs"]["train"], meta0["inputs"]["train"]);
    assert_eq!(m["humans"].as_object().unwrap().keys().collect::<Vec<_>>(), ["train"]);
    assert_eq!(m["planned_splits"], json!(["train"]));
    assert!(m.get("coverage").is_none());
    assert_eq!(rec["source"]["plan_sha256"], sha_file(&src.join("plan.json")));
    assert_eq!(rec["source"]["items_sha256"], sha_file(&src.join("items.jsonl")));
    assert_eq!(o["plan_sha256"], sha_file(&copy.join("plan.json")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(copy.join("items.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // refusals: dev_select and test as the split (nothing written), a copy of a copy, an existing output,
    // an unreadable split line (by line number), a changed source file, an unknown overlap extra
    for split in ["dev_select", "dev-select", "test"] {
        let out = d.join(format!("refused-{split}"));
        let o = run(&["split-copy", "--plan", s(&src), "--split", split, "--out", s(&out)]);
        assert!(!o.status.success() && err(&o).contains("refused"), "{split}: {}", err(&o));
        assert!(!out.exists());
    }
    let o = run(&["split-copy", "--plan", s(&copy), "--split", "train", "--out", s(&d.join("cc"))]);
    assert!(!o.status.success() && err(&o).contains("already a split copy"), "{}", err(&o));
    let o = run(&["split-copy", "--plan", s(&src), "--split", "train", "--out", s(&copy)]);
    assert!(!o.status.success() && err(&o).contains("exists"), "{}", err(&o));
    // the source records no CAL input, so the copy keeps none (amendment 24); dev_iter is not kept
    assert_eq!(rec["overlap_extras"], json!([]));
    assert_eq!(rec["source"]["inputs_not_kept"], json!(["dev_iter"]));
    let bad = d.join("plan-unreadable");
    copy_dir(&pdir, &bad);
    let n_items = std::fs::read_to_string(bad.join("items.jsonl")).unwrap().lines().count();
    append(&bad.join("items.jsonl"), b"{\"item_id\":\"no-split\"}\n");
    rehash(&bad);
    let o = run(&["split-copy", "--plan", s(&bad), "--split", "train", "--out", s(&d.join("cu"))]);
    assert!(!o.status.success() && err(&o).contains(&format!("line {}", n_items + 1)) && !d.join("cu").exists(), "{}", err(&o));
    append(&bad.join("plan-failures.jsonl"), b"\n");
    let o = run(&["split-copy", "--plan", s(&bad), "--split", "train", "--out", s(&d.join("cd"))]);
    assert!(!o.status.success() && err(&o).contains("changed since planning"), "{}", err(&o));
    // replan-check refuses a copy (it re-plans from every split's input)
    let o = run(&["replan-check", "--plan", s(&copy), "--out", s(&d.join("rp"))]);
    assert!(!o.status.success() && err(&o).contains("split copy"), "{}", err(&o));

    // generate: the two-split request file is refused before any parse, with its line number and the split's class
    let cfg = d.join("cfg.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": g, "allowed_splits": ["train", "dev_iter", "dev_select", "test"]})).unwrap()).unwrap();
    let missing = d.join("missing");
    let gen_run = |plan: &Path, extra: &[&str]| {
        Command::new(BIN)
            .args(["generate", "--plan", s(plan), "--generator", &g, "--config", s(&cfg), "--runs", s(&d.join("runs")), "--index", s(&missing)])
            .args(["--tokenizer", s(&missing), "--dry-run"])
            .args(extra)
            .output()
            .unwrap()
    };
    let rf = src.join("requests").join(format!("{g}.jsonl"));
    let lines: Vec<Value> = std::fs::read_to_string(&rf).unwrap().lines().take_while(|l| !l.contains('\0')).map(|l| serde_json::from_str(l).unwrap()).collect();
    let first = lines[0]["split"].as_str().unwrap().to_string();
    let at = lines.iter().position(|v| v["split"] != first.as_str()).unwrap_or(lines.len()) + 1;
    let o = gen_run(&src, &["--splits", "train"]);
    assert!(!o.status.success() && err(&o).contains(&format!("line {at}:")) && err(&o).contains("holds one split") && !err(&o).contains("never parsed"), "{}", err(&o));
    // a request file of the junk lines only after one split: the class names the refused split
    let only = d.join("plan-junk-tail");
    copy_dir(&copy, &only);
    append(&only.join("requests").join(format!("{g}.jsonl")), JUNK);
    rehash(&only);
    let o = gen_run(&only, &["--splits", "train"]);
    assert!(!o.status.success() && err(&o).contains("a refused split (old Dev-select or the consumed Test) (dev_select)"), "{}", err(&o));
    // B5b review finding 3: a request, items or plan-failures file whose one split is dev_select or test is
    // refused at its first line before any parse, by generate, finalize and pilot
    let (configs, tok) = (d.join("configs"), tokenizer(d));
    std::fs::create_dir_all(&configs).unwrap();
    std::fs::create_dir_all(d.join("runs")).unwrap();
    let fin = |plan: &Path, out: &str| run(&["finalize", "--plan", s(plan), "--runs", s(&d.join("runs")), "--configs", s(&configs), "--tokenizer", s(&tok), "--out", s(&d.join(out))]);
    let refused = |o: &Output, what: &str| {
        let e = err(o);
        assert!(!o.status.success() && e.contains(&format!("{what} line 1:")) && e.contains("a refused split") && !e.contains("key must be a string") && !e.contains("never parsed"), "{e}");
    };
    for split in ["test", "dev_select"] {
        let rq = d.join(format!("plan-requests-{split}-only"));
        copy_dir(&copy, &rq);
        std::fs::write(rq.join("requests").join(format!("{g}.jsonl")), refused_only(split)).unwrap();
        rehash(&rq);
        refused(&gen_run(&rq, &["--splits", "train"]), &format!("{g}.jsonl"));
        refused(&fin(&rq, &format!("fin-rq-{split}")), &format!("{g}.jsonl"));
        let it = d.join(format!("plan-items-{split}-only"));
        copy_dir(&copy, &it);
        std::fs::write(it.join("items.jsonl"), refused_only(split)).unwrap();
        rehash(&it);
        refused(&gen_run(&it, &["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"]), "items.jsonl");
        refused(&fin(&it, &format!("fin-it-{split}")), "items.jsonl");
        refused(&run(&["pilot", "--plan", s(&it), "--step", "D4", "--out", s(&d.join(format!("pilot-{split}")))]), "items.jsonl");
        assert!(!d.join(format!("pilot-{split}")).exists());
        let pf = d.join(format!("plan-failures-{split}-only"));
        copy_dir(&copy, &pf);
        std::fs::write(pf.join("plan-failures.jsonl"), refused_only(split)).unwrap();
        rehash(&pf);
        refused(&fin(&pf, &format!("fin-pf-{split}")), "plan-failures.jsonl");
    }
    // mutation R8: generate --profile refuses a run of another split than the plan's items
    let o = gen_run(&copy, &["--splits", "dev_iter", "--profile", "cc_cohort_profile_v2", "--profile-population", "deviter2_t"]);
    assert!(!o.status.success() && err(&o).contains("the plan's items are of split Some(\"train\"), not the run's dev_iter"), "{}", err(&o));
    // under --profile, a two-split items file is refused before any parse
    let it2 = d.join("plan-items-two");
    copy_dir(&copy, &it2);
    let n = std::fs::read_to_string(it2.join("items.jsonl")).unwrap().lines().count();
    append(&it2.join("items.jsonl"), JUNK);
    rehash(&it2);
    let on = ["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"];
    let o = gen_run(&it2, &on);
    assert!(!o.status.success() && err(&o).contains("items.jsonl line") && err(&o).contains(&format!("line {}:", n + 1)), "{}", err(&o));
    // a one-split copy runs, profile off and on
    ok(&gen_run(&copy, &["--splits", "train"]));
    let v = ok(&gen_run(&copy, &on));
    assert_eq!(v["profile"]["population"], "train2");
    // a recorded dev_select input that would be read (plans v1 and v2 read it; v3 never does) is refused
    // before it is opened
    let ds = d.join("plan-dev-select-input");
    copy_dir(&copy, &ds);
    let mut m2 = read_json(&ds.join("plan.json"));
    m2["inputs"]["dev_select"] = json!({"path": s(&missing), "sha256": "0".repeat(64)});
    std::fs::write(ds.join("plan.json"), serde_json::to_vec_pretty(&m2).unwrap()).unwrap();
    ok(&gen_run(&ds, &["--splits", "train"]));
    m2["schema"] = json!(plan::PLAN_SCHEMA);
    std::fs::write(ds.join("plan.json"), serde_json::to_vec_pretty(&m2).unwrap()).unwrap();
    let o = gen_run(&ds, &["--splits", "train"]);
    assert!(!o.status.success() && err(&o).contains("dev_select input"), "{}", err(&o));
    // finalize refuses the two-split plan before any parse, and runs on the copy
    let o = fin(&src, "fin-src");
    assert!(!o.status.success() && err(&o).contains("holds one split") && !err(&o).contains("never parsed"), "{}", err(&o));
    // a plan whose items and requests hold one split but whose plan failures hold two is refused too
    let pf = d.join("plan-failures-two");
    copy_dir(&copy, &pf);
    std::fs::write(pf.join("plan-failures.jsonl"), b"{\"split\":\"train\",\"item_id\":\"a\"}\n{\"split\":\"dev_iter\",\x00 never parsed\n").unwrap();
    rehash(&pf);
    let o = fin(&pf, "fin-pf");
    assert!(!o.status.success() && err(&o).contains("plan-failures.jsonl line 2:") && !err(&o).contains("never parsed"), "{}", err(&o));
    // mutation R9: items of one split with a request file, or plan failures, of another are refused
    let other = gens.iter().find(|h| !lines_of(&pdir.join("requests").join(format!("{h}.jsonl")), "dev_iter").is_empty()).unwrap().clone();
    let rx = d.join("plan-request-other-split");
    copy_dir(&copy, &rx);
    std::fs::write(rx.join("requests").join(format!("{other}.jsonl")), lines_of(&pdir.join("requests").join(format!("{other}.jsonl")), "dev_iter")).unwrap();
    rehash(&rx);
    let o = fin(&rx, "fin-rx");
    assert!(!o.status.success() && err(&o).contains("holds split dev_iter, items.jsonl split train"), "{}", err(&o));
    let fx = d.join("plan-failures-other-split");
    copy_dir(&copy, &fx);
    std::fs::write(fx.join("plan-failures.jsonl"), b"{\"split\":\"dev_iter\",\"item_id\":\"a\"}\n").unwrap();
    rehash(&fx);
    let o = fin(&fx, "fin-fx");
    assert!(!o.status.success() && err(&o).contains("plan-failures.jsonl holds split dev_iter, items.jsonl split train"), "{}", err(&o));
    ok(&fin(&copy, "fin-copy"));
    let sum = read_json(&d.join("fin-copy").join("summary.json"));
    assert_eq!(sum["splits"].as_object().unwrap().keys().collect::<Vec<_>>(), ["train"]);
    // B5b review finding 9: a plan-wide plan-failure line (plan v2's ladder relaxation count) is passed over by
    // finalize as split-copy passes over it, not refused
    let pw = d.join("plan-failures-plan-wide");
    copy_dir(&copy, &pw);
    append(&pw.join("plan-failures.jsonl"), b"{\"stage\":\"plan\",\"reason\":\"ladder_cluster_distinctness_relaxed\",\"count\":3}\n");
    rehash(&pw);
    ok(&fin(&pw, "fin-pw"));
}

/// A llama-server stand-in: `/props`, `/v1/models`, and for every chat call a
/// distinct prose reply of `words` words (11 per sentence).
fn serve(words: usize) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || {
        let mut n = 0u64;
        for st in listener.incoming() {
            let mut st = st.unwrap();
            let Some((line, _)) = read_http(&mut st) else { continue };
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

fn quit(endpoint: &str) {
    TcpStream::connect(endpoint.trim_start_matches("http://")).unwrap().write_all(b"QUIT / HTTP/1.1\r\n\r\n").unwrap();
}

#[test]
fn d4_mirror_plan_regenerates_mirrors_in_renorms_batch_with_their_remaining_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let p = |n: &str| d.join(n);
    for (split, n) in [("train", 8), ("dev_iter", 2), ("dev_select", 2), ("cal", 2)] {
        write_humans(&p(&format!("{split}.jsonl")), split, n);
    }
    // a plan v1 (all four splits, as R0's), which re-plans byte for byte with B5b's options off
    let pdir = p("plan-v1");
    ok(&run(&["plan", "--train", s(&p("train.jsonl")), "--dev-iter", s(&p("dev_iter.jsonl")), "--dev-select", s(&p("dev_select.jsonl")), "--cal", s(&p("cal.jsonl")), "--out", s(&pdir)]));
    ok(&run(&["replan-check", "--plan", s(&pdir), "--out", s(&p("replan"))]));
    let rc = read_json(&p("replan").join("replan-check.json"));
    assert_eq!(rc["identical"], true, "{rc}");
    assert!(!std::fs::read_to_string(pdir.join("items.jsonl")).unwrap().contains("attempt_limit"));
    let copy = p("copy-train");
    ok(&run(&["split-copy", "--plan", s(&pdir), "--split", "train", "--out", s(&copy)]));
    // the copy keeps the source's CAL input as generate's overlap extra (R0's screen; amendment 24)
    let cm = read_json(&copy.join("plan.json"));
    assert_eq!(cm["inputs"].as_object().unwrap().keys().collect::<Vec<_>>(), ["cal", "train"]);
    assert_eq!(cm["inputs"]["cal"], read_json(&pdir.join("plan.json"))["inputs"]["cal"]);
    assert_eq!(cm["split_copy"]["overlap_extras"], json!(["cal"]));
    assert_eq!(cm["split_copy"]["source"]["inputs_not_kept"], json!(["dev_iter", "dev_select"]));
    assert!(!std::fs::read_to_string(copy.join("items.jsonl")).unwrap().contains("\"split\":\"cal\""));
    // the generator with the most Train mirrors
    let items = jsonl(&copy.join("items.jsonl"));
    let mut mirrors: BTreeMap<String, usize> = BTreeMap::new();
    for it in items.iter().filter(|i| i["kind"] == "mirror") {
        *mirrors.entry(it["generator"].as_str().unwrap().to_string()).or_default() += 1;
    }
    let g = mirrors.iter().max_by_key(|(g, n)| (**n, std::cmp::Reverse((*g).clone()))).unwrap().0.clone();
    // profile-off journals on the copy: every reply is 99 words, accepted for a 121-word parent's mirror
    // and rejected under profile v2 by the final-document window (150 words)
    let (endpoint, server) = serve(99);
    let cfg = p("cfg.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({"key": g, "allowed_splits": ["train"], "endpoint": endpoint, "model_path": "/models/m.gguf", "alias": "m"})).unwrap()).unwrap();
    let (idx, tok) = (index(d), tokenizer(d));
    let gen_run = |plan: &Path, runs: &Path, extra: &[&str]| {
        Command::new(BIN)
            .args(["generate", "--plan", s(plan), "--generator", &g, "--config", s(&cfg), "--runs", s(runs), "--index", s(&idx), "--tokenizer", s(&tok)])
            .args(["--workers", "1", "--splits", "train"])
            .args(extra)
            .output()
            .unwrap()
    };
    let runs = p("runs");
    ok(&gen_run(&copy, &runs, &[]));
    // renorm on the four-split plan (it reads lines by their split bytes), then D4's plan on the copy
    let out = p("renorm");
    ok(&run(&["renorm", "--plan", s(&pdir), "--runs", s(&runs), "--split", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2",
        "--tokenizer", s(&tok), "--out", s(&out)]));
    let rg = jsonl(&out.join("regeneration-plan-split-train.jsonl"));
    let mrows: Vec<&Value> = rg.iter().filter(|r| r["action"] == "regenerate_mirror").collect();
    assert!(!mrows.is_empty(), "{rg:?}");
    // r05 is read from VALUES-R2.json (S0-6), never the command line; absent, the plan is refused
    let vals = values_file(d, "values.json", json!({"S0-6": {"id": "S0-6", "name": "r05", "value": "0.8"}}));
    let no_s06 = values_file(d, "values-no-s06.json", json!({"S0-7": {"id": "S0-7", "name": "v_fam", "value": 5}}));
    let o = run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&out), "--split", "train", "--values", s(&no_s06), "--out", s(&p("d4-no-s06"))]);
    assert!(!o.status.success() && err(&o).contains("holds no S0-6") && !p("d4-no-s06").exists(), "{}", err(&o));
    let d4 = p("d4");
    let o = run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&out), "--split", "train", "--values", s(&vals), "--out", s(&d4)]);
    let rec = ok(&o);
    assert_eq!(rec["regenerated_mirrors"].as_u64(), Some(mrows.len() as u64));
    let m = read_json(&d4.join("plan.json"));
    assert_eq!(m["items_sha256"], sha_file(&d4.join("items.jsonl")));
    assert_eq!(m["mirror_regeneration"]["mirror_target"]["floor_words"], 188);
    assert_eq!(m["mirror_regeneration"]["values"]["sha256"], sha_file(&vals));
    assert_eq!(m["mirror_regeneration"]["values"]["value"], "0.8");
    assert_eq!(m["inputs"]["cal"], cm["inputs"]["cal"]);
    // the new requests: mirror_target_words_v2 (max(121, ceil(150 / 0.8) = 188)), the old prompt with only
    // its length changed, the remaining attempts, a digest and seed of their own
    let old_reqs: HashMap<String, Value> = jsonl(&copy.join("requests").join(format!("{g}.jsonl"))).into_iter().map(|r| (r["request_id"].as_str().unwrap().to_string(), r)).collect();
    let new_reqs: HashMap<String, Value> = jsonl(&d4.join("requests").join(format!("{g}.jsonl"))).into_iter().map(|r| (r["request_id"].as_str().unwrap().to_string(), r)).collect();
    let humans: HashMap<String, Value> = jsonl(&p("train.jsonl")).into_iter().map(|h| (h["id"].as_str().unwrap().to_string(), h)).collect();
    let rows = jsonl(&d4.join("mirror-regeneration.jsonl"));
    assert_eq!(rows.len(), mrows.len());
    let mut mirror_items = BTreeSet::new();
    for r in &rows {
        let old = &old_reqs[r["old_request_id"].as_str().unwrap()];
        let new = &new_reqs[r["new_request_id"].as_str().unwrap()];
        assert!(!new_reqs.contains_key(r["old_request_id"].as_str().unwrap()));
        let pw = humans[old["parent_id"].as_str().unwrap()]["word_count"].as_u64().unwrap();
        assert_eq!(old["target_words"].as_u64(), Some(pw));
        let want = pw.max(188);
        assert_eq!(new["target_words"].as_u64(), Some(want));
        assert_eq!(new["prompt"].as_str().unwrap(), old["prompt"].as_str().unwrap().replace(&format!(" about {pw} words long."), &format!(" about {want} words long.")));
        assert_ne!(new["prompt"], old["prompt"]);
        let row = mrows.iter().find(|x| x["request_id"] == old["request_id"]).unwrap();
        assert_eq!(new["attempt_limit"], row["attempts_remaining"]);
        assert_eq!(new["attempt_limit"].as_u64(), Some(1));
        let req: plan::Request = serde_json::from_value(new.clone()).unwrap();
        let sha = plan::request_sha(&req.generator, &req.endpoint, &req.template_id, &req.prompt, &req.decoding);
        assert_eq!((req.request_sha256.as_str(), req.seed), (sha.as_str(), plan::seed_of(&sha)));
        assert_eq!(new["request_id"].as_str().unwrap(), format!("req-{}", &text::sha_hex(format!("{}:mirror_target_words_v2", old["request_id"].as_str().unwrap()).as_bytes())[..24]));
        for k in ["item_id", "parent_id", "split", "generator", "endpoint", "template_id", "decoding", "span_index", "cohort"] {
            assert_eq!(new[k], old[k], "{k}");
        }
        mirror_items.insert(old["item_id"].as_str().unwrap().to_string());
    }
    // every other line is the copy's, byte for byte; a regenerated mirror item names its new request
    let cl: Vec<String> = std::fs::read_to_string(copy.join("items.jsonl")).unwrap().lines().map(String::from).collect();
    let dl: Vec<String> = std::fs::read_to_string(d4.join("items.jsonl")).unwrap().lines().map(String::from).collect();
    assert_eq!(cl.len(), dl.len());
    for (a, b) in cl.iter().zip(&dl) {
        let v: Value = serde_json::from_str(a).unwrap();
        if mirror_items.contains(v["item_id"].as_str().unwrap()) {
            let w: Value = serde_json::from_str(b).unwrap();
            assert_eq!(w["requests"].as_array().unwrap().len(), 1);
            assert!(new_reqs.contains_key(w["requests"][0].as_str().unwrap()));
            assert_eq!((w["parent_id"].clone(), w["generator"].clone(), w["decoding"].clone()), (v["parent_id"].clone(), v["generator"].clone(), v["decoding"].clone()));
        } else {
            assert_eq!(a, b);
        }
    }
    // D4's order: the regenerate items and the regenerated mirrors
    let d4_order: BTreeSet<String> = std::fs::read_to_string(d4.join(format!("d4-order-{g}.txt"))).unwrap().lines().map(String::from).collect();
    let mut want: BTreeSet<String> = rg.iter().filter(|r| r["action"] == "regenerate" && r["generator"] == g.as_str()).map(|r| r["item_id"].as_str().unwrap().to_string()).collect();
    want.extend(mirror_items.iter().cloned());
    assert_eq!(d4_order, want);
    // the D4 plan resumes in renorm's batch: same binding, prior and runs; the new mirror requests are pending
    let on = ["--profile", "cc_cohort_profile_v2", "--profile-population", "train2"];
    let order = d4.join("d4-order-{GEN}.txt");
    let dry = ok(&gen_run(&d4, &out, &[&on[..], &["--request-order", s(&order), "--dry-run"][..]].concat()));
    let sum = read_json(&out.join("RENORM-split-train.json"));
    assert_eq!(dry["profile"], sum["profile"]);
    let listed: BTreeSet<String> = dry["item_order"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    assert!(mirror_items.is_subset(&listed));
    // the keyed pilot of D4: below the floor in every cell, so the whole step is its pilot
    let pil = p("pilot");
    ok(&run(&["pilot", "--plan", s(&d4), "--request-order", s(&order), "--step", "D4", "--out", s(&pil)]));
    let pr = read_json(&pil.join("PILOT.json"));
    assert_eq!((pr["rows"].as_u64(), pr["whole_step_is_pilot"].as_bool()), (Some(want.len() as u64), Some(true)));
    assert_eq!(std::fs::read(pil.join(format!("pilot-order-{g}.txt"))).unwrap(), std::fs::read(d4.join(format!("d4-order-{g}.txt"))).unwrap());
    assert!(pr["cells"].as_object().unwrap().keys().any(|k| k.ends_with("|model_only")));
    assert!(!pr["cells_below_floor"].as_array().unwrap().is_empty());
    // mutation R13: an order file listing another generator's item is refused
    let other_item = jsonl(&d4.join("items.jsonl")).into_iter().find(|i| i["generator"] != g.as_str()).unwrap();
    std::fs::write(p(&format!("wrong-{g}.txt")), format!("{}\n", other_item["item_id"].as_str().unwrap())).unwrap();
    let o = run(&["pilot", "--plan", s(&d4), "--request-order", s(&p("wrong-{GEN}.txt")), "--step", "D4", "--out", s(&p("pilot-wrong"))]);
    assert!(!o.status.success() && err(&o).contains(&format!("is {}'s, not {g}'s", other_item["generator"].as_str().unwrap())) && !p("pilot-wrong").exists(), "{}", err(&o));
    // attempts stop at the carried limit (1): the mirrors run once, are rejected by the window, and are done
    let morder = p("mirrors-{GEN}.txt");
    std::fs::write(p(&format!("mirrors-{g}.txt")), mirror_items.iter().map(|i| format!("{i}\n")).collect::<String>()).unwrap();
    ok(&gen_run(&d4, &out, &[&on[..], &["--request-order", s(&morder)][..]].concat()));
    let states = generation::load_states(&[out.join(&g)]).unwrap();
    for r in &rows {
        let st = &states[r["new_request_id"].as_str().unwrap()];
        assert_eq!((st.intents.clone(), st.responses.len()), (vec![0], 1), "{r}");
        // the carried limit is journaled, so journal-only readers see the request done (finding 6)
        assert!(st.accepted().is_none() && st.done_within(1) && !st.done_within(2));
        assert_eq!(st.limit, Some(1));
        assert!(st.done());
    }
    // rates counts them terminal under their carried limit: terminal = not accepted with the request's own limit
    // used (finalize's reading), the limit read from D4's request file
    let rates = ok(&run(&["rates", "--runs", s(&out)]));
    let lim_of: HashMap<String, u64> = jsonl(&d4.join("requests").join(format!("{g}.jsonl"))).into_iter()
        .map(|r| (r["request_id"].as_str().unwrap().to_string(), r["attempt_limit"].as_u64().unwrap_or(2))).collect();
    let terminal = states.iter().filter(|(id, st)| st.accepted().is_none() && u64::from(st.used()) >= *lim_of.get(id.as_str()).unwrap_or(&2)).count();
    assert!(terminal >= rows.len());
    assert_eq!(rates[&g]["terminal_failed"].as_u64(), Some(terminal as u64), "{}", rates[&g]);
    let again = ok(&gen_run(&d4, &out, &[&on[..], &["--request-order", s(&morder), "--dry-run"][..]].concat()));
    assert_eq!(again["pending_before"], 0, "{again}");
    quit(&endpoint);
    server.join().unwrap();
    // finalize on D4's plan: the regenerated mirrors are terminal failures within their limit
    let configs = p("configs");
    std::fs::create_dir_all(&configs).unwrap();
    std::fs::copy(&cfg, configs.join("g.json")).unwrap();
    ok(&run(&["finalize", "--plan", s(&d4), "--runs", s(&out), "--configs", s(&configs), "--tokenizer", s(&tok), "--out", s(&p("fin"))]));
    let fails = jsonl(&p("fin").join("train").join("failures.jsonl"));
    for id in &mirror_items {
        assert!(fails.iter().any(|f| f["item_id"] == id.as_str() && f["status"] == "terminal_failure"), "{id}");
    }
    // a regenerate_mirror row with no attempt left is refused (case 0): drop_out_of_attempts gets no request
    let bad = p("renorm-bad");
    copy_dir(&out, &bad);
    let rp = bad.join("regeneration-plan-split-train.jsonl");
    let tampered: String = std::fs::read_to_string(&rp)
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["action"] == "regenerate_mirror" {
                v["attempts_used"] = json!(2);
                v["attempts_remaining"] = json!(0);
            }
            format!("{}\n", serde_json::to_string(&v).unwrap())
        })
        .collect();
    std::fs::write(&rp, &tampered).unwrap();
    let mut sm = read_json(&bad.join("RENORM-split-train.json"));
    sm["outputs"]["regeneration_plan"]["sha256"] = json!(text::sha_hex(tampered.as_bytes()));
    std::fs::write(bad.join("RENORM-split-train.json"), serde_json::to_vec_pretty(&sm).unwrap()).unwrap();
    let o = run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&bad), "--split", "train", "--values", s(&vals), "--out", s(&p("d4-bad"))]);
    assert!(!o.status.success() && err(&o).contains("attempt rule") && !p("d4-bad").exists(), "{}", err(&o));
    // another batch's record is refused: a population other than renorm's
    let mut sm2 = read_json(&out.join("RENORM-split-train.json"));
    let other = p("renorm-other");
    copy_dir(&out, &other);
    sm2["profile"]["prior_sha256"] = json!("0".repeat(64));
    std::fs::write(other.join("RENORM-split-train.json"), serde_json::to_vec_pretty(&sm2).unwrap()).unwrap();
    let o = run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&other), "--split", "train", "--values", s(&vals), "--out", s(&p("d4-other"))]);
    assert!(!o.status.success() && err(&o).contains("batch record differs"), "{}", err(&o));
    let bad_r = values_file(d, "values-bad-r05.json", json!({"S0-6": {"id": "S0-6", "name": "r05", "value": "0.6e0"}}));
    let o = run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&out), "--split", "train", "--values", s(&bad_r), "--out", s(&p("d4-r"))]);
    assert!(!o.status.success() && err(&o).contains("r05"), "{}", err(&o));
    // mutations R10 and R12: renorm's summary naming another plan, or another input digest, is refused
    let tamper = |name: &str, f: &dyn Fn(&mut Value)| -> Output {
        let t = p(name);
        copy_dir(&out, &t);
        let mut v = read_json(&t.join("RENORM-split-train.json"));
        f(&mut v);
        std::fs::write(t.join("RENORM-split-train.json"), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&t), "--split", "train", "--values", s(&vals), "--out", s(&p(&format!("{name}-d4")))])
    };
    let o = tamper("renorm-plan", &|v: &mut Value| v["inputs"]["plan_sha256"] = json!("0".repeat(64)));
    assert!(!o.status.success() && err(&o).contains("renorm read another plan"), "{}", err(&o));
    let o = tamper("renorm-input", &|v: &mut Value| v["inputs"]["input"]["sha256"] = json!("0".repeat(64)));
    assert!(!o.status.success() && err(&o).contains("renorm read another train input"), "{}", err(&o));
    // mutation R4: an order file of renorm's that is not its regenerate rows' items (digest re-recorded)
    let ro = p("renorm-order");
    copy_dir(&out, &ro);
    let mut v = read_json(&ro.join("RENORM-split-train.json"));
    let name = format!("tampered-order-{g}.txt");
    let extra = mirror_items.iter().next().unwrap().clone();
    let body = match v["outputs"]["request_orders"][&g]["path"].as_str() {
        Some(op) => format!("{}{extra}\n", std::fs::read_to_string(ro.join(op)).unwrap()),
        None => format!("{extra}\n"),
    };
    std::fs::write(ro.join(&name), &body).unwrap();
    v["outputs"]["request_orders"][&g] = json!({"path": name, "sha256": text::sha_hex(body.as_bytes())});
    std::fs::write(ro.join("RENORM-split-train.json"), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let o = run(&["mirror-plan", "--plan", s(&copy), "--renorm", s(&ro), "--split", "train", "--values", s(&vals), "--out", s(&p("d4-order"))]);
    assert!(!o.status.success() && err(&o).contains(&format!("renorm's order file of {g} is not its regenerate rows' items")) && !p("d4-order").exists(), "{}", err(&o));
    // mutations R1 and R1b: renorm reads each request's own limit; mirrors whose requests carry a limit of 1, with
    // their one attempt used, are dropped out of attempts, and the rows report that limit
    let lim = p("copy-limit-1");
    copy_dir(&copy, &lim);
    let rf = lim.join("requests").join(format!("{g}.jsonl"));
    let mirror_reqs: BTreeSet<String> = mrows.iter().map(|r| r["request_id"].as_str().unwrap().to_string()).collect();
    let body: String = std::fs::read_to_string(&rf).unwrap().lines().map(|l| {
        let mut v: Value = serde_json::from_str(l).unwrap();
        if mirror_reqs.contains(v["request_id"].as_str().unwrap()) {
            v["attempt_limit"] = json!(1);
        }
        format!("{}\n", serde_json::to_string(&v).unwrap())
    }).collect();
    std::fs::write(&rf, body).unwrap();
    rehash(&lim);
    let out1 = p("renorm-limit-1");
    ok(&run(&["renorm", "--plan", s(&lim), "--runs", s(&runs), "--split", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2",
        "--tokenizer", s(&tok), "--out", s(&out1)]));
    let rg1 = jsonl(&out1.join("regeneration-plan-split-train.jsonl"));
    for rid in &mirror_reqs {
        let r = rg1.iter().find(|r| r["request_id"] == rid.as_str()).unwrap();
        assert_eq!((r["action"].as_str(), r["attempt_limit"].as_u64(), r["attempts_used"].as_u64(), r["attempts_remaining"].as_u64()),
            (Some("drop_out_of_attempts"), Some(1), Some(1), Some(0)), "{r}");
    }
    assert!(rg1.iter().all(|r| mirror_reqs.contains(r["request_id"].as_str().unwrap()) || r["attempt_limit"] == 2));
    assert!(read_json(&out1.join("RENORM-split-train.json"))["rules"]["attempts"].as_str().unwrap().contains("the request's limit"));
}

#[test]
fn stratify_train_parses_only_train_lines_and_budgets_targets() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let tr = d.join("train.jsonl");
    write_humans(&tr, "train", 12);
    let pdir = d.join("base");
    ok(&run(&["plan-v3", "--train", s(&tr), "--base-generators", "granite_base,smollm3_base,ernie_base", "--out", s(&pdir)]));
    append(&pdir.join("items.jsonl"), JUNK);
    let runs = d.join("runs");
    std::fs::create_dir_all(runs.join("granite_base")).unwrap();
    // one accepted granite item, and old Dev-select journal lines that are never parsed
    let it: Value = std::fs::read_to_string(pdir.join("items.jsonl")).unwrap().lines().filter(|l| !l.contains('\0') && !l.contains("broken")).map(|l| serde_json::from_str::<Value>(l).unwrap()).find(|v| v["generator"] == "granite_base").unwrap();
    let rid = it["requests"][0].as_str().unwrap();
    let mut j = format!("{}\n{}\n", json!({"kind":"intent","request_id":rid,"split":"train","attempt":0}), json!({"kind":"response","request_id":rid,"split":"train","attempt":0,"verdict":{"accepted":true,"text":"Synthetic words."}}));
    j.push_str("{\"kind\":\"response\",\"split\":\"dev_select\",\"verdict\":\x00 never parsed\n");
    std::fs::write(runs.join("granite_base").join("journal-1-w0.jsonl"), j).unwrap();
    let parts = d.join("parts.jsonl");
    std::fs::write(&parts, "").unwrap();
    // v_fam is read from VALUES-R2.json (S0-7), never the command line (B5b review finding 5)
    let vals = values_file(d, "values.json", json!({"S0-7": {"id": "S0-7", "name": "v_fam", "value": 5}}));
    let out = d.join("sel");
    ok(&run(&["stratify-train", "--plan", s(&pdir), "--runs", s(&runs), "--partitions", s(&parts), "--cohort-shares", "train_base_v3=1",
        "--target", "granite_base", "--values", s(&vals), "--generator", "ernie_base=2", "--floor", "0", "--seed", "synthetic", "--out", s(&out)]));
    let sel = read_json(&out.join("selection.json"));
    assert_eq!(sel["targets"]["granite_base"], json!({"target_accepted": 5, "existing_accepted_train": 1, "budget": 4}));
    assert_eq!((sel["target_values"]["sha256"].as_str(), sel["target_values"]["value"].as_u64()), (Some(sha_file(&vals).as_str()), Some(5)));
    let refused_out = d.join("sel-refused");
    for (extra, want) in [(vec!["--target", "granite_base"], "--values"), (vec!["--target", "granite_base=5", "--values", s(&vals)], "a generator name"),
        (vec!["--target", "granite_base", "--values", s(&values_file(d, "v-empty.json", json!({})))], "holds no S0-7")] {
        let mut a = vec!["stratify-train", "--plan", s(&pdir), "--runs", s(&runs), "--partitions", s(&parts), "--cohort-shares", "train_base_v3=1",
            "--seed", "x", "--out", s(&refused_out)];
        a.extend(extra);
        let o = run(&a);
        assert!(!o.status.success() && err(&o).contains(want) && !refused_out.exists(), "{want}: {}", err(&o));
    }
    assert_eq!(sel["generators"]["granite_base"]["budget"], 4);
    assert_eq!(sel["generators"]["granite_base"]["items"], 4);
    let sk = &sel["lines_of_other_splits_skipped_unparsed"];
    let items_key = format!("{}:items", pdir.display());
    let j_key = format!("{}:journals", runs.display());
    assert_eq!((sk[&items_key]["dev_select"].as_u64(), sk[&items_key]["test"].as_u64()), (Some(1), Some(1)), "{sk}");
    assert_eq!(sk[&j_key]["dev_select"].as_u64(), Some(1), "{sk}");
    let o = run(&["stratify-train", "--plan", s(&pdir), "--runs", s(&runs), "--partitions", s(&parts), "--cohort-shares", "train_base_v3=1",
        "--target", "granite_base", "--values", s(&vals), "--generator", "granite_base=2", "--seed", "x", "--out", s(&d.join("sel2"))]);
    assert!(!o.status.success() && err(&o).contains("both a budget and a target"), "{}", err(&o));
}

/// A synthetic enron-v1 pool under `root` and its S0-9 record: 99 admitted
/// writer keys (buckets below 900) with two documents each, 3 reserved keys
/// (900..1000), the pool files and their SHA256SUMS. Train-half documents are
/// whole rows; diagnostic-half lines carry an unparsable tail.
struct EnronFixture {
    pool: PathBuf,
    s09: Value,
    train: Vec<String>,
}

fn enron_fixture(root: &Path) -> EnronFixture {
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
    let tset: BTreeSet<&String> = train.iter().collect();
    let pool = root.join("pools").join("enron-v1");
    std::fs::create_dir_all(&pool).unwrap();
    let mut docs = Vec::new();
    let mut words = 0usize;
    for (i, k) in admitted.iter().enumerate() {
        for j in 0..2 {
            if tset.contains(k) {
                let t = prose(&SYN, (i * 2 + j) as u64, 4, 11);
                words += text::words(&t);
                let row = json!({"id": format!("pool-enron-{i}-{j}"), "collection": "enron_email_historical", "domain": "email_corporate", "writer_key": k,
                    "origin": "human_only", "text": t, "text_sha256": text::sha_hex(t.as_bytes()), "word_count": text::words(&t), "source_group": format!("g-{i}-{j}")});
                docs.extend_from_slice(serde_json::to_string(&row).unwrap().as_bytes());
            } else {
                docs.extend_from_slice(format!("{{\"collection\":\"enron_email_historical\",\"writer_key\":\"{k}\",\x00 never parsed").as_bytes());
            }
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
    EnronFixture { pool, s09, train: train.to_vec() }
}

#[test]
fn enron_half_writes_the_v23_train_half_from_the_bound_pool_and_never_parses_the_diagnostic_half() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let f = enron_fixture(&root);
    let vp = values_file(dir.path(), "values.json", json!({"S0-9": f.s09}));
    let enron = |pool: &Path, values: &Path, out: &str| run(&["enron-half", "--pool", s(pool), "--root", s(&root), "--values", s(values), "--out", s(&dir.path().join(out))]);
    let o = enron(&f.pool, &vp, "train-half.jsonl");
    let rec = ok(&o);
    assert!(!String::from_utf8_lossy(&o.stdout).contains("enron_mailbox:"));
    assert_eq!((rec["train_half"]["documents"].as_u64(), rec["train_half"]["writer_keys"].as_u64()), (Some(100), Some(50)));
    assert_eq!(rec["train_half"]["words"], f.s09["train_half"]["words"]);
    assert_eq!(rec["documents"]["sha256"], sha_file(&f.pool.join("documents.jsonl")));
    let rows = jsonl(&dir.path().join("train-half.jsonl"));
    assert_eq!(rows.len(), 100);
    let tset: BTreeSet<&String> = f.train.iter().collect();
    assert!(rows.iter().all(|r| r["frozen_split"] == "train" && tset.contains(&r["writer_key"].as_str().unwrap().to_string())));
    // B5b review finding 8 and mutation R3: every binding is checked before the output is written
    let refuse = |name: &str, s09: Value, pool: &Path, want: &str| {
        let vp = values_file(dir.path(), &format!("{name}.json"), json!({"S0-9": s09}));
        let o = enron(pool, &vp, name);
        assert!(!o.status.success() && err(&o).contains(want) && !dir.path().join(name).exists(), "{name}: {}", err(&o));
        assert!(!err(&o).contains("enron_mailbox:"), "{name}: a key printed");
    };
    let mut m = f.s09.clone();
    m["train_half"]["documents"] = json!(99);
    refuse("docs-train", m, &f.pool, "the Train half holds 100 documents");
    let mut m = f.s09.clone();
    m["diagnostic_half"]["documents"] = json!(97);
    refuse("docs-diag", m, &f.pool, "diagnostic half's document count");
    let mut m = f.s09.clone();
    m["train_half"]["words"] = json!(f.s09["train_half"]["words"].as_u64().unwrap() + 1);
    refuse("words", m, &f.pool, "words, S0-9 records another count");
    let mut m = f.s09.clone();
    m["inputs"][3]["sha256"] = json!("0".repeat(64));
    refuse("sums", m, &f.pool, "S0-9 binds");
    // a pool other than the one S0-9 names under the root, even with the same files
    let other = root.join("pools").join("enron-copy");
    copy_dir(&f.pool, &other);
    refuse("other-pool", f.s09.clone(), &other, "is not the one S0-9 names under the root");
    // documents.jsonl changed after admission: its SHA256SUMS digest no longer matches
    let changed = dir.path().join("repo-changed");
    copy_dir(&root, &changed);
    let cp = changed.join("pools").join("enron-v1");
    append(&cp.join("documents.jsonl"), b"\n");
    let vp2 = values_file(dir.path(), "changed-values.json", json!({"S0-9": f.s09}));
    let o = run(&["enron-half", "--pool", s(&cp), "--root", s(&changed), "--values", s(&vp2), "--out", s(&dir.path().join("changed-out"))]);
    assert!(!o.status.success() && err(&o).contains("documents.jsonl") && !dir.path().join("changed-out").exists(), "{}", err(&o));
    // a file S0-9 also binds whose SHA256SUMS digest differs from S0-9's
    let mut m = f.s09.clone();
    m["inputs"][0]["sha256"] = json!("0".repeat(64));
    refuse("admission", m, &f.pool, "ADMISSION.json: the pool's SHA256SUMS and S0-9 disagree");
    // halves that differ from S0-9's lists (V23's order broken) are refused by the S0-9 check
    let mut m = f.s09.clone();
    let mut t: Vec<Value> = m["train_half"]["writer_keys_in_plan_key_order"].as_array().unwrap().clone();
    t.reverse();
    m["train_half"]["list_sha256"] = json!(text::sha_hex(t.iter().map(|k| format!("{}\n", k.as_str().unwrap())).collect::<String>().as_bytes()));
    m["train_half"]["writer_keys_in_plan_key_order"] = json!(t);
    refuse("order", m, &f.pool, "plan-key order");
}

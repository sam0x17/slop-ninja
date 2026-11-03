//! `slop_ninja_a_positives renorm` (B4) on synthetic text: a plan's
//! profile-off journals, written by `generate` against a local stand-in
//! server, are re-verdicted under profile v2 with no server running, and the
//! output resumes under `generate --profile` with the same batch record.
use grammar_a_positives::{generation, text};
use serde_json::{Value, json};
use std::collections::BTreeSet;
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

fn write_humans(path: &Path, split: &str) {
    let mut out = String::new();
    for (d, coll) in [("fw", "fineweb_cc_historical"), ("wp", "wikipedia_enwiki_historical")] {
        for k in 0..8u64 {
            let t = format!("{}\n\n{}", prose(&SYN, 100 + k, 6, 11), prose(&SYN, 200 + k, 7, 11));
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
/// that writes the journals: `/props`, `/v1/models`, and a 77-word chat reply
/// (plan v3's continuation target on these humans): a list of seven lines
/// for odd calls, which the profile-off checks accept and profile v2's form
/// screen rejects, and seven prose sentences for even ones.
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
                    prose(&ALT, 400 + n, 7, 11)
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

#[test]
fn renorm_rereads_profile_off_journals_and_resumes_under_generate_profile() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let (tr, di) = (d.join("train.jsonl"), d.join("dev-iter.jsonl"));
    write_humans(&tr, "train");
    write_humans(&di, "dev_iter");
    let pdir = d.join("plan");
    let o = std::process::Command::new(BIN)
        .args(["plan-v3", "--train", tr.to_str().unwrap(), "--dev-iter", di.to_str().unwrap(), "--splits", "train,dev_iter", "--out", pdir.to_str().unwrap()])
        .output()
        .unwrap();
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
    // B5b (ruling R2): generate refuses the two-split plan, so its journals are
    // written on one-split copies (split-copy), and D4's resume runs on the
    // train copy with renorm's output runs (recorded change)
    let (copy_t, copy_d) = (d.join("plan-train"), d.join("plan-dev-iter"));
    for (split, c) in [("train", &copy_t), ("dev_iter", &copy_d)] {
        let o = std::process::Command::new(BIN)
            .args(["split-copy", "--plan", pdir.to_str().unwrap(), "--split", split, "--out", c.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let gen_cmd = |runs: &Path, extra: &[&str]| {
        let mut c = std::process::Command::new(BIN);
        let plan = if extra.contains(&"dev_iter") { &copy_d } else { &copy_t };
        c.args(["generate", "--plan", plan.to_str().unwrap(), "--generator", generator.as_str(), "--config", cfg.to_str().unwrap()])
            .args(["--runs", runs.to_str().unwrap(), "--index", idx.to_str().unwrap(), "--tokenizer", tok.to_str().unwrap(), "--workers", "1"])
            .args(extra);
        c
    };
    let err = |o: &std::process::Output| String::from_utf8_lossy(&o.stderr).into_owned();
    // the profile-off journals (train and dev_iter)
    for split in ["train", "dev_iter"] {
        let o = gen_cmd(&runs, &["--splits", split, "--limit", "8"]).output().unwrap();
        assert!(o.status.success(), "{}", err(&o));
    }
    TcpStream::connect(endpoint.trim_start_matches("http://")).unwrap().write_all(b"QUIT / HTTP/1.1\r\n\r\n").unwrap();
    server.join().unwrap();
    let input = generation::load_states(&[runs.join(&generator)]).unwrap();
    let mut train_ids: BTreeSet<String> = BTreeSet::new();
    for l in jsonl(&pdir.join("requests").join(format!("{generator}.jsonl"))) {
        if l["split"] == "train" {
            train_ids.insert(l["request_id"].as_str().unwrap().to_string());
        }
    }
    let accepted_in = train_ids.iter().filter(|id| input.get(*id).is_some_and(|s| s.accepted().is_some())).count();
    assert!(accepted_in > 0);
    // renorm: no server is running now
    let out = d.join("renorm");
    let renorm = |extra: &[&str], out: &Path| {
        std::process::Command::new(BIN)
            .args(["renorm", "--plan", pdir.to_str().unwrap(), "--runs", runs.to_str().unwrap(), "--out", out.to_str().unwrap()])
            .args(["--profile", "cc_cohort_profile_v2", "--profile-population", "train2"])
            .args(extra)
            .output()
            .unwrap()
    };
    for (extra, want) in [
        (&["--population", "train"][..], "takes --split and --tokenizer"),
        (&["--split", "train"][..], "takes --split and --tokenizer"),
    ] {
        let o = renorm(extra, &d.join("bad"));
        assert!(!o.status.success() && err(&o).contains(want), "{extra:?}: {}", err(&o));
    }
    let o = renorm(&["--split", "train", "--tokenizer", tok.to_str().unwrap()], &out);
    assert!(o.status.success(), "{}", err(&o));
    let sum: Value = serde_json::from_slice(&std::fs::read(out.join("RENORM-split-train.json")).unwrap()).unwrap();
    assert_eq!(sum["counts"]["requests_reverdicted"].as_u64(), Some(accepted_in as u64));
    assert_eq!(sum["counts"]["raw_body_check"], json!({"equal": accepted_in}));
    // dev_iter lines are another split: skipped before they are parsed
    assert!(sum["counts"]["journal_lines_other_splits_skipped_unparsed"]["dev_iter"].as_u64().unwrap() > 0, "{}", sum["counts"]);
    assert_eq!(sum["counts"]["journal_lines_outside_batch"], 0);
    assert!(sum["counts"]["plan_lines_skipped_unparsed"]["requests"]["dev_iter"].as_u64().unwrap() > 0, "{}", sum["counts"]);
    // the output resumes under generate --profile: the binding is the run's record, and the
    // pending requests are those not done in the re-verdict journal
    let on = ["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "train2"];
    let o = gen_cmd(&out, &on).arg("--dry-run").output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let dry: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(dry["profile"]["prior_sha256"], sum["prior_sha256"]);
    let states = generation::load_states(&[out.join(&generator)]).unwrap();
    let pending = train_ids.iter().filter(|id| !states.get(*id).is_some_and(|s| s.done())).count();
    assert_eq!(dry["pending_before"].as_u64(), Some(pending as u64));
    let order: BTreeSet<String> = dry["item_order"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
    let plan = jsonl(&out.join("regeneration-plan-split-train.jsonl"));
    assert!(plan.iter().any(|r| r["action"] == "regenerate" && r["reasons"].to_string().contains("form_line_list_like")), "{plan:?}");
    for r in plan {
        assert_eq!(r["action"] == "regenerate", order.contains(r["item_id"].as_str().unwrap()), "{r}");
        assert!(!states[r["request_id"].as_str().unwrap()].accepted().is_some());
    }
    // D4's resume with the request order runs only the regenerate items, in plan order
    let order_file = out.join(format!("regenerate-order-{generator}.txt"));
    let listed: Vec<String> = std::fs::read_to_string(&order_file).unwrap().lines().map(String::from).collect();
    let regen: BTreeSet<String> = jsonl(&out.join("regeneration-plan-split-train.jsonl")).iter().filter(|r| r["action"] == "regenerate").map(|r| r["item_id"].as_str().unwrap().to_string()).collect();
    assert_eq!(listed.iter().cloned().collect::<BTreeSet<_>>(), regen);
    let ordered = out.join("regenerate-order-{GEN}.txt");
    let o = gen_cmd(&out, &on).args(["--request-order", ordered.to_str().unwrap()]).arg("--dry-run").output().unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let dry2: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(dry2["item_order"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>(), listed);
    // only the listed items' requests are pending; without the order every request not done is
    let listed_reqs = jsonl(&pdir.join("items.jsonl")).iter().filter(|i| listed.contains(&i["item_id"].as_str().unwrap().to_string())).map(|i| i["requests"].as_array().unwrap().len()).sum::<usize>();
    assert!(dry2["pending_before"].as_u64().unwrap() as usize <= listed_reqs && dry2["pending_before"].as_u64() <= dry["pending_before"].as_u64(), "{dry2}");
    // dev_select and test are refused before anything is read
    for split in ["dev_select", "test"] {
        let o = renorm(&["--split", split, "--tokenizer", tok.to_str().unwrap()], &d.join(format!("refused-{split}")));
        assert!(!o.status.success() && err(&o).contains("refused"), "{split}: {}", err(&o));
        assert!(!d.join(format!("refused-{split}")).exists());
    }
    // every accepted verdict of the output is a profile v2 verdict with its raw child
    for st in states.values() {
        if let Some(v) = st.accepted() {
            assert_eq!(v["verdict"]["diag"]["post"]["prior_sha256"], sum["prior_sha256"]);
            assert!(v["verdict"]["raw_text"].is_string());
        }
    }
    // another record, or the profile off, is refused on the output
    for (extra, want) in [
        (&["--splits", "train", "--profile", "cc_cohort_profile_v2", "--profile-population", "cal2_t"][..], "another profile record"),
        (&["--splits", "train"][..], "profile off"),
    ] {
        let o = gen_cmd(&out, extra).arg("--dry-run").output().unwrap();
        assert!(!o.status.success() && err(&o).contains(want), "{extra:?}: {}", err(&o));
    }
    // a plan and journals that hold old Dev-select and consumed-Test lines (plans v1 and v2 do), deliberately
    // unparsable after their split keys: renorm reads them past by their split bytes and parses none
    let p2 = d.join("plan-mixed");
    copy_dir(&pdir, &p2);
    let junk = b"{\"split\":\"dev_select\",\"item_id\":\"x\",\x00 never parsed\n{\"id\":\"t\", \"split\" : \"test\", broken\n";
    append(&p2.join("items.jsonl"), junk);
    let rf = p2.join("requests").join(format!("{generator}.jsonl"));
    append(&rf, junk);
    let tr2 = d.join("train-mixed.jsonl");
    std::fs::copy(&tr, &tr2).unwrap();
    append(&tr2, b"{\"frozen_split\":\"dev_select\",\"text\":\x00 never parsed\n");
    let mut m2: Value = serde_json::from_slice(&std::fs::read(p2.join("plan.json")).unwrap()).unwrap();
    m2["items_sha256"] = json!(text::sha_hex(&std::fs::read(p2.join("items.jsonl")).unwrap()));
    m2["request_files"][&generator]["sha256"] = json!(text::sha_hex(&std::fs::read(&rf).unwrap()));
    m2["inputs"]["train"] = json!({"path": tr2, "sha256": text::sha_hex(&std::fs::read(&tr2).unwrap())});
    std::fs::write(p2.join("plan.json"), serde_json::to_vec(&m2).unwrap()).unwrap();
    let runs2 = d.join("runs-mixed");
    copy_dir(&runs, &runs2);
    let jf = std::fs::read_dir(runs2.join(&generator)).unwrap().map(|e| e.unwrap().path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("journal-")).unwrap();
    append(&jf, b"{\"kind\":\"response\",\"split\":\"dev_select\",\"verdict\":\x00 never parsed\n{\"split\": \"test\", broken\n");
    let out2 = d.join("renorm-mixed");
    let o = std::process::Command::new(BIN)
        .args(["renorm", "--plan", p2.to_str().unwrap(), "--runs", runs2.to_str().unwrap(), "--out", out2.to_str().unwrap()])
        .args(["--profile", "cc_cohort_profile_v2", "--profile-population", "train2", "--split", "train", "--tokenizer", tok.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let sum2: Value = serde_json::from_slice(&std::fs::read(out2.join("RENORM-split-train.json")).unwrap()).unwrap();
    let sk = &sum2["counts"]["plan_lines_skipped_unparsed"];
    for (input, split) in [("items", "dev_select"), ("items", "test"), ("requests", "dev_select"), ("requests", "test"), ("input", "dev_select")] {
        assert_eq!(sk[input][split], 1, "{input} {split}: {sk}");
    }
    let js = &sum2["counts"]["journal_lines_other_splits_skipped_unparsed"];
    assert_eq!((js["dev_select"].as_u64(), js["test"].as_u64()), (Some(1), Some(1)), "{js}");
    // the re-verdict is the same as on the plan without those lines
    for f in ["reverdict-split-train.jsonl", "regeneration-plan-split-train.jsonl"] {
        assert_eq!(std::fs::read(out.join(f)).unwrap(), std::fs::read(out2.join(f)).unwrap(), "{f}");
    }
    // renorm refuses an existing output directory
    let o = renorm(&["--split", "train", "--tokenizer", tok.to_str().unwrap()], &out);
    assert!(!o.status.success() && err(&o).contains("exists"), "{}", err(&o));
}

//! `plan-v3` and `replan-check` run as the binary on synthetic inputs (no
//! corpus text, no model or network call):
//! - `--dev-select` is refused in both modes, and base-model mode refuses
//!   every input but Train and CAL, before any file is opened or written;
//! - a base-model plan is Train only and re-plans byte for byte;
//! - `replan-check` never opens a `dev_select` input recorded by a v3 plan.
use grammar_a_positives::text;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_a_positives");

/// Synthetic vocabulary (no corpus text).
const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
    "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
];

fn passage(seed: usize, n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let len = 6 + (seed * 5 + i * 11) % 13;
        let ws: Vec<&str> = (0..len).map(|k| SYN[(seed * 29 + i * 13 + k * 7) % SYN.len()]).collect();
        let s = ws.join(" ");
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&s[..1].to_uppercase());
        out.push_str(&s[1..]);
        out.push('.');
    }
    out
}

/// Synthetic human rows of one split, in three domains.
fn write_humans(path: &Path, split: &str, base: usize) {
    let mut b = String::new();
    for (d, coll) in [("fw", "fineweb_cc_historical"), ("wp", "wikipedia_enwiki_historical"), ("rd", "reddit_webis_tldr17_historical")] {
        for k in 0..12 {
            let seed = base + k;
            let t = passage(seed, 2 + k % 7);
            let row = json!({"id":format!("{split}-{d}-{seed}"),"frozen_split":split,"origin":"human_only","text":t,
                "text_sha256":text::sha_hex(t.as_bytes()),"word_count":text::words(&t),"collection":coll,
                "provenance":{"title":format!("synthetic {seed}")},"source_group":format!("g-{d}-{}", seed / 2),"domain":"web"});
            b.push_str(&serde_json::to_string(&row).unwrap());
            b.push('\n');
        }
    }
    std::fs::write(path, b).unwrap();
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Every refusal fires before a file is opened: the input paths do not
/// exist, so an earlier open would fail with a different error, and the
/// output directory is never created.
#[test]
fn plan_v3_refuses_dev_select_and_non_train_base_inputs_before_opening_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = |n: &str| dir.path().join(format!("missing-{n}.jsonl"));
    let (tr, di, ds, ca) = (missing("train"), missing("dev-iter"), missing("dev-select"), missing("cal"));
    let out = dir.path().join("plan");
    let base = "granite_base,smollm3_base,ernie_base";
    let cases: &[(&[&str], &str)] = &[
        (&["--dev-select", s(&ds)], "refuses --dev-select"),
        (&["--dev-iter", s(&di), "--dev-select", s(&ds), "--cal", s(&ca)], "refuses --dev-select"),
        (&["--dev-select", s(&ds), "--splits", "train"], "refuses --dev-select"),
        (&["--base-generators", base, "--dev-select", s(&ds)], "refuses --dev-select"),
        (&["--base-generators", base, "--dev-iter", s(&di)], "refuses --dev-iter"),
        (&["--base-generators", base, "--dev-iter", s(&di), "--splits", "train,dev_iter"], "refuses --dev-iter"),
        (&["--base-generators", base, "--splits", "train,dev_iter"], "plans Train only"),
        (&["--base-generators", base, "--prefix-every", "2"], "base-model mode plans none"),
        (&["--base-generators", "olmo2_base"], "not a Train base family"),
        (&["--splits", "train,dev_iter"], "planned split dev_iter has no input"),
    ];
    for &(extra, want) in cases {
        let mut args = vec!["plan-v3", "--train", s(&tr), "--out", s(&out)];
        args.extend(extra.iter().copied());
        let o = run(&args);
        let e = stderr(&o);
        assert!(!o.status.success(), "{args:?} succeeded");
        assert!(e.contains(want), "{args:?}: {e}");
        assert!(!e.contains("No such file"), "{args:?} opened a file first: {e}");
        assert!(!out.exists(), "{args:?} wrote its output directory");
    }
}

fn plan(dir: &Path, name: &str, args: &[&str]) -> (PathBuf, Value) {
    let out = dir.join(name);
    let mut a = vec!["plan-v3", "--out", s(&out)];
    a.extend_from_slice(args);
    let o = run(&a);
    assert!(o.status.success(), "{a:?}: {}", stderr(&o));
    let meta: Value = serde_json::from_slice(&std::fs::read(out.join("plan.json")).unwrap()).unwrap();
    (out, meta)
}

fn replan(pdir: &Path, out: &Path) -> Value {
    let o = run(&["replan-check", "--plan", s(pdir), "--out", s(out)]);
    assert!(o.status.success(), "replan-check {}: {}", pdir.display(), stderr(&o));
    serde_json::from_slice(&std::fs::read(out.join("replan-check.json")).unwrap()).unwrap()
}

fn same_bytes(a: &Path, b: &Path) {
    assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap(), "{} != {}", a.display(), b.display());
}

/// A base-model plan reads Train (and CAL when given) only, holds only the
/// base cohort on `/completion`, and re-plans byte for byte from its record.
#[test]
fn base_plan_is_train_only_and_replans_byte_identically() {
    let dir = tempfile::tempdir().unwrap();
    let (tr, ca) = (dir.path().join("train.jsonl"), dir.path().join("cal.jsonl"));
    write_humans(&tr, "train", 0);
    write_humans(&ca, "cal", 500);
    let gens = ["granite_base", "smollm3_base", "ernie_base"];
    for (name, extra) in [("base", &[][..]), ("base-cal", &["--cal", s(&ca)][..])] {
        let mut args = vec!["--train", s(&tr), "--base-generators", "ernie_base,granite_base,smollm3_base"];
        args.extend(extra.iter().copied());
        let (pdir, meta) = plan(dir.path(), name, &args);
        assert_eq!(meta["mode"], "base_model");
        assert_eq!(meta["base_generators"], json!(gens));
        assert_eq!(meta["planned_splits"], json!(["train"]));
        let want_inputs: &[&str] = if name == "base-cal" { &["cal", "train"] } else { &["train"] };
        let mut got: Vec<&str> = meta["inputs"].as_object().unwrap().keys().map(String::as_str).collect();
        got.sort_unstable();
        assert_eq!(got, want_inputs);
        assert_eq!(meta["humans"]["train"].as_object().unwrap().values().map(|v| v.as_u64().unwrap()).sum::<u64>(), 36);
        let items: Vec<Value> = std::fs::read_to_string(pdir.join("items.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert!(!items.is_empty() && items.iter().all(|i| i["cohort"] == "train_base_v3" && i["split"] == "train"));
        assert_eq!(items.len() + meta["plan_failures"].as_u64().unwrap() as usize, 36);
        let files: Vec<String> = meta["request_files"].as_object().unwrap().keys().cloned().collect();
        assert!(!files.is_empty() && files.iter().all(|g| gens.contains(&g.as_str())), "{files:?}");
        for g in &files {
            for l in std::fs::read_to_string(pdir.join("requests").join(format!("{g}.jsonl"))).unwrap().lines() {
                let r: Value = serde_json::from_str(l).unwrap();
                assert_eq!((r["endpoint"].as_str(), r["generator"].as_str(), r["cohort"].as_str()), (Some("completion"), Some(g.as_str()), Some("train_base_v3")));
            }
        }
        let new = dir.path().join(format!("{name}-replan"));
        let rec = replan(&pdir, &new);
        assert_eq!(rec["identical"], true, "{rec}");
        assert_eq!(rec["recorded_inputs_not_read"], json!([]));
        same_bytes(&pdir.join("items.jsonl"), &new.join("items.jsonl"));
        same_bytes(&pdir.join("plan-failures.jsonl"), &new.join("plan-failures.jsonl"));
        for g in &files {
            let f = format!("requests/{g}.jsonl");
            same_bytes(&pdir.join(&f), &new.join(&f));
        }
        let replanned: Value = serde_json::from_slice(&std::fs::read(new.join("plan.json")).unwrap()).unwrap();
        for k in ["mode", "base_generators", "inputs", "coverage", "plan_failures_by_reason"] {
            assert_eq!(replanned[k], meta[k], "{k}");
        }
    }
}

/// A v3 plan written before the refusal recorded a `dev_select` input. Its
/// re-plan does not open it (the recorded path does not exist) and matches.
#[test]
fn replan_check_never_opens_a_recorded_dev_select() {
    let dir = tempfile::tempdir().unwrap();
    let (tr, ca) = (dir.path().join("train.jsonl"), dir.path().join("cal.jsonl"));
    write_humans(&tr, "train", 0);
    write_humans(&ca, "cal", 500);
    let (pdir, mut meta) = plan(dir.path(), "instruct", &["--train", s(&tr), "--cal", s(&ca)]);
    assert!(meta["items"].as_u64().unwrap() > 0);
    let missing = dir.path().join("missing-dev-select.jsonl");
    meta["inputs"]["dev_select"] = json!({"path":s(&missing),"sha256":"0".repeat(64)});
    std::fs::write(pdir.join("plan.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    let rec = replan(&pdir, &dir.path().join("instruct-replan"));
    assert_eq!(rec["identical"], true, "{rec}");
    assert_eq!(rec["recorded_inputs_not_read"], json!(["dev_select"]));
    assert!(!missing.exists());
}

fn read_rows(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn order_ids(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap().lines().filter(|l| !l.starts_with('#')).map(String::from).collect()
}

/// `stratify-train` credits the base cohort's accepted rows by kind: after a
/// pass with one item per domain is accepted, the next pass picks the other
/// kind in every domain that still has it (the plain rotation would pick the
/// first kind again).
#[test]
fn stratify_train_balances_base_kinds_across_passes() {
    let dir = tempfile::tempdir().unwrap();
    let tr = dir.path().join("train.jsonl");
    write_humans(&tr, "train", 0);
    let (pdir, _) = plan(dir.path(), "base", &["--train", s(&tr), "--base-generators", "granite_base,smollm3_base,ernie_base"]);
    let items = read_rows(&pdir.join("items.jsonl"));
    let by_id: std::collections::HashMap<&str, &Value> = items.iter().map(|i| (i["item_id"].as_str().unwrap(), i)).collect();
    let (runs, parts) = (dir.path().join("runs"), dir.path().join("partitions.jsonl"));
    std::fs::create_dir_all(runs.join("pass1")).unwrap();
    std::fs::write(&parts, "").unwrap();
    let gens = ["granite_base", "smollm3_base", "ernie_base"];
    let pass = |out: &Path| {
        let mut a = vec!["stratify-train", "--plan", s(&pdir), "--runs", s(&runs), "--partitions", s(&parts), "--cohort-shares", "train_base_v3=1"];
        let budgets: Vec<String> = gens.iter().map(|g| format!("{g}=3")).collect();
        for b in &budgets {
            a.extend(["--generator", b.as_str()]);
        }
        a.extend(["--floor", "0", "--seed", "synthetic", "--out", s(out)]);
        let o = run(&a);
        assert!(o.status.success(), "{}", stderr(&o));
        serde_json::from_slice::<Value>(&std::fs::read(out.join("selection.json")).unwrap()).unwrap()
    };
    let (o1, o2) = (dir.path().join("o1"), dir.path().join("o2"));
    pass(&o1);
    // accept every item of pass 1 (a synthetic journal, no model call)
    let mut journal = String::new();
    let mut first: std::collections::BTreeMap<(String, String), String> = std::collections::BTreeMap::new();
    for g in gens {
        for id in order_ids(&o1.join(format!("{g}.txt"))) {
            let it = by_id[id.as_str()];
            assert!(first.insert((g.to_owned(), it["domain"].as_str().unwrap().to_owned()), it["kind"].as_str().unwrap().to_owned()).is_none(), "one item per domain");
            for r in it["requests"].as_array().unwrap() {
                journal.push_str(&json!({"kind":"intent","request_id":r,"attempt":0}).to_string());
                journal.push('\n');
                journal.push_str(&json!({"kind":"response","request_id":r,"attempt":0,"verdict":{"accepted":true,"text":"Synthetic words."}}).to_string());
                journal.push('\n');
            }
        }
    }
    std::fs::write(runs.join("pass1").join("journal-1-w0.jsonl"), journal).unwrap();
    let sel2 = pass(&o2);
    let mut checked = 0;
    for g in gens {
        let credit = &sel2["generators"][g]["balanced_kinds"]["accepted_credit"];
        for ((gg, d), k) in first.iter().filter(|((gg, _), _)| gg == g) {
            assert_eq!(credit[format!("{d}|train_base_v3|{k}")], json!(1), "{gg} {d}");
            let other = if k == "continuation" { "splice" } else { "continuation" };
            let left = items.iter().filter(|i| i["generator"] == g && i["domain"] == d.as_str() && i["kind"] == other).count();
            let picked: Vec<String> = order_ids(&o2.join(format!("{g}.txt")))
                .iter()
                .filter(|id| by_id[id.as_str()]["domain"] == d.as_str())
                .map(|id| by_id[id.as_str()]["kind"].as_str().unwrap().to_owned())
                .collect();
            if left > 0 && !picked.is_empty() {
                assert_eq!(picked[0], other, "{g} {d}: the kind with fewer accepted rows first");
                checked += 1;
            }
        }
    }
    assert!(checked >= 3, "{checked} cells checked");
}

/// `finalize` on a v3 plan that recorded a `dev_select` input never opens it
/// (the recorded path does not exist) and writes no Dev-select output.
#[test]
fn finalize_never_opens_a_recorded_dev_select() {
    let dir = tempfile::tempdir().unwrap();
    let (tr, ca) = (dir.path().join("train.jsonl"), dir.path().join("cal.jsonl"));
    write_humans(&tr, "train", 0);
    write_humans(&ca, "cal", 500);
    let (pdir, mut meta) = plan(dir.path(), "instruct", &["--train", s(&tr), "--cal", s(&ca)]);
    let missing = dir.path().join("missing-dev-select.jsonl");
    meta["inputs"]["dev_select"] = json!({"path":s(&missing),"sha256":"0".repeat(64)});
    std::fs::write(pdir.join("plan.json"), serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
    let (runs, configs, tok, out) = (dir.path().join("runs"), dir.path().join("configs"), dir.path().join("tokenizer.json"), dir.path().join("final"));
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::create_dir_all(&configs).unwrap();
    std::fs::write(&tok, r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#).unwrap();
    let o = run(&["finalize", "--plan", s(&pdir), "--runs", s(&runs), "--configs", s(&configs), "--tokenizer", s(&tok), "--out", s(&out)]);
    assert!(o.status.success(), "{}", stderr(&o));
    let summary: Value = serde_json::from_slice(&std::fs::read(out.join("summary.json")).unwrap()).unwrap();
    let mut splits: Vec<&str> = summary["splits"].as_object().unwrap().keys().map(String::as_str).collect();
    splits.sort_unstable();
    assert_eq!(splits, ["cal", "train"]);
    assert!(out.join("train").join("failures.jsonl").exists() && !out.join("dev-select").exists() && !missing.exists());
}

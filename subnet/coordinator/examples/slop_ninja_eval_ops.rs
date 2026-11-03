//! Operator tooling for the fan-out rehearsals on testnet: load plans, gates,
//! and content-free summaries of status documents, tails and package
//! components. It never talks to the network; the operator's shell fetches the
//! documents (status with the bearer token in a stdin header) and passes their
//! files here.
//!
//! ```text
//! slop_ninja_eval_ops load-plan --label L --cohort N --copies C --max-lanes M --budget-usd B
//!     --source-label S --source-index INDEX.json --source-tasks T --reference ARTIFACT.json
//!     [--faults FAULTS.json] [--lane-docs D] [--lease-ms MS] [--step-budget-ms MS]
//!     [--failures-before-pause K]
//! slop_ninja_eval_ops gate --for deploy|load MAIN_STATUS.json LOAD_STATUS.json
//! slop_ninja_eval_ops summary STATUS.json LABEL
//! slop_ninja_eval_ops watch-line STATUS.json LABEL     (exit 3 once the cohort is past scoring or faulted)
//! slop_ninja_eval_ops tail-stats TAIL.json
//! slop_ninja_eval_ops snapshot-args PLAN.json
//! slop_ninja_eval_ops keys-check KEYS.json PACKAGE_KEY RECEIPT_KEY INSTANCE
//! slop_ninja_eval_ops copy-ties COMPONENTS.json [--require-ties]
//! slop_ninja_eval_ops chunk-keys CHUNK_MAP.json       (each object's key below cohorts/<label>/)
//! slop_ninja_eval_ops report-summary VERIFY_REPORT.json
//! ```
//!
//! - `load-plan` writes a `slop_ninja_eval_load_plan_v1` plan: R_0 (the
//!   reference artifact, without its owner, from ARTIFACT.json) owned by
//!   synthetic identity 0, plus C copies of it owned by synthetic identities
//!   1..=C that are never registered (hotkey = SHA-256 of
//!   `slop_ninja.load/synthetic-identity/v1/{uid}`, registration block
//!   1,000,000 + uid). FAULTS.json is a `LoadFaults` object. The plan is
//!   checked with the coordinator's own parser before it is printed.
//! - `gate` refuses (exit 2) a deploy while either instance has a cohort
//!   generating, sealing, scoring or packaging, and a load run also while the
//!   main instance has a cohort queued or in admission (DESIGN §9-10: no
//!   authoritative cohort generates or scores during a load run).
//! - `copy-ties` compares every job whose package is the reference's with the
//!   reference job: utilities equal, above or below, and the largest
//!   difference. With `--require-ties` any difference exits 1.
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_coordinator::eval::plan::{Ceilings, LoadPlan};
use std::collections::BTreeMap;
use std::process::exit;

fn read_json(path: &str) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| fail(&format!("read {path}: {e}")));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

fn fail(msg: &str) -> ! {
    eprintln!("slop_ninja_eval_ops: {msg}");
    exit(2)
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Flags as `--name value`; positional arguments in order.
fn parse(args: &[String]) -> (BTreeMap<String, String>, Vec<String>) {
    let (mut flags, mut pos) = (BTreeMap::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.strip_prefix("--") {
            Some("require-ties") => {
                flags.insert("require-ties".into(), String::new());
            }
            Some(k) => {
                let v = it.next().unwrap_or_else(|| fail(&format!("--{k} needs a value")));
                flags.insert(k.to_owned(), v.clone());
            }
            None => pos.push(a.clone()),
        }
    }
    (flags, pos)
}

fn flag<T: std::str::FromStr>(flags: &BTreeMap<String, String>, k: &str) -> T {
    flags.get(k).unwrap_or_else(|| fail(&format!("--{k} is required"))).parse().unwrap_or_else(|_| fail(&format!("--{k}")))
}

fn opt<T: std::str::FromStr>(flags: &BTreeMap<String, String>, k: &str) -> Option<T> {
    flags.get(k).map(|v| v.parse().unwrap_or_else(|_| fail(&format!("--{k}"))))
}

/// A synthetic, never-registered identity for load plans.
fn ident(uid: u16) -> Value {
    let h = sha_hex(format!("slop_ninja.load/synthetic-identity/v1/{uid}").as_bytes());
    json!({"uid": uid, "hotkey": format!("0x{h}"), "registration_block": 1_000_000 + u64::from(uid)})
}

fn load_plan(flags: &BTreeMap<String, String>) {
    let copies: u16 = flag(flags, "copies");
    let budget_usd: f64 = flag(flags, "budget-usd");
    let source_label: String = flag(flags, "source-label");
    let index = std::fs::read(flag::<String>(flags, "source-index")).unwrap_or_else(|e| fail(&format!("source index: {e}")));
    let mut art = read_json(&flag::<String>(flags, "reference"));
    art.as_object_mut().unwrap_or_else(|| fail("reference artifact is not an object")).remove("owner");
    let with_owner = |uid: u16| {
        let mut a = art.clone();
        a["owner"] = ident(uid);
        a
    };
    let faults = flags.get("faults").map(|f| read_json(f)).unwrap_or_else(|| json!({}));
    let plan = json!({
        "schema": "slop_ninja_eval_load_plan_v1",
        "cohort": flag::<u64>(flags, "cohort"),
        "label": flag::<String>(flags, "label"),
        "source": {
            "label": source_label,
            "index_key": format!("cohorts/{source_label}/batch/index.json"),
            "index_sha256": sha_hex(&index),
            "tasks": flag::<u32>(flags, "source-tasks"),
        },
        "max_attempts": 3,
        "burn_identity": ident(0),
        "allow_list": (1..=copies).map(ident).collect::<Vec<_>>(),
        "reference": with_owner(0),
        "entrants": (1..=copies).map(with_owner).collect::<Vec<_>>(),
        "scorer": {"chunk_docs": 32},
        "fanout": {
            "score": {
                "lane_docs": opt::<u32>(flags, "lane-docs").unwrap_or(200),
                "max_lanes": flag::<u32>(flags, "max-lanes"),
                "lease_ms": opt::<u64>(flags, "lease-ms").unwrap_or(60_000),
                "isolation_lease_ms": 240_000,
                "scoring_budget_microusd": (budget_usd * 1e6).round() as u64,
                "prestart_lead_ms": 0,
                "prestart_wait_ms": 120_000,
                "inactivity_ms": 60_000,
                "failures_before_pause": opt::<u32>(flags, "failures-before-pause").unwrap_or(200),
                "step_budget_ms": opt::<u64>(flags, "step-budget-ms").unwrap_or(600_000),
                "doc_timeout_ms": 60_000,
                "artifact_max_mib": 640,
                "load_ms_per_mib": 150,
                "artifact_failures_before_model_error": 3,
            },
            "generation": {"max_lanes": 1, "items_per_lane": 8, "inflight_per_lane": 4, "lease_ms": 120_000, "max_throttle_ms": 900_000},
            "package": {"max_lanes": 64},
            "cost": {"instance_nanousd_per_s": 31_400, "vcpu_nanousd_per_s": 20_000, "vcpus": 4,
                "lane_do_nanousd_per_s": 1_563, "doc_ub_nanousd": 40_000, "cold_ub_nanousd": 2_500_000},
        },
        "faults": faults,
    });
    let bytes = serde_json::to_vec_pretty(&plan).expect("plan json");
    if let Err(e) = LoadPlan::parse(&bytes, Ceilings::default()) {
        fail(&format!("the coordinator refuses this plan: {}", e.0));
    }
    println!("{}", String::from_utf8(bytes).expect("utf-8"));
}

const WORK: [&str; 4] = ["generating", "sealing", "scoring", "packaging"];

fn gate(flags: &BTreeMap<String, String>, pos: &[String]) {
    let what = flags.get("for").map(String::as_str).unwrap_or_else(|| fail("--for deploy|load"));
    let [main, load] = pos else { fail("gate --for deploy|load MAIN_STATUS.json LOAD_STATUS.json") };
    let mut refuse = Vec::new();
    for (instance, path) in [("main", main), ("load", load)] {
        let st = read_json(path);
        let cohorts = st["cohorts"].as_array().unwrap_or_else(|| fail(&format!("{path}: no cohorts")));
        for c in cohorts {
            let phase = c["phase"].as_str().unwrap_or("");
            let label = c["label"].as_str().unwrap_or("");
            let waiting = instance == "main" && what == "load" && matches!(phase, "queued" | "admission");
            if WORK.contains(&phase) || waiting {
                refuse.push(format!("{instance}: {label} is {phase}"));
            }
        }
    }
    if refuse.is_empty() {
        println!("gate {what}: clear");
    } else {
        println!("gate {what}: refused ({})", refuse.join("; "));
        exit(2);
    }
}

fn cohort<'v>(st: &'v Value, label: &str) -> &'v Value {
    st["cohorts"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["label"] == label))
        .unwrap_or_else(|| fail(&format!("no cohort {label}")))
}

fn secs(a: &Value, b: &Value) -> Value {
    match (a.as_f64(), b.as_f64()) {
        (Some(a), Some(b)) => json!((a - b) / 1000.0),
        _ => Value::Null,
    }
}

fn summary(pos: &[String]) {
    let [path, label] = pos else { fail("summary STATUS.json LABEL") };
    let st = read_json(path);
    let c = cohort(&st, label);
    let ln = &c["lanes"];
    let t = &ln["timeline_ms"];
    let log: BTreeMap<String, Value> = c["log"]
        .as_array()
        .map(|l| l.iter().filter_map(|e| Some((e["phase"].as_str()?.to_owned(), e["at_ms"].clone()))).collect())
        .unwrap_or_default();
    let s = &ln["kinds"]["score"];
    println!("label {} jobs {} {} receipts {} chunk_attempts {}", c["label"], c["job_count"], c["job_status"], c["receipts_recorded"], c["chunk_attempts"]);
    println!(
        "queued->scoring {} s; scoring wall {} s; packaging {} s",
        secs(&t["scoring_started"], log.get("queued").unwrap_or(&Value::Null)),
        secs(&t["scoring_finished"], &t["scoring_started"]),
        secs(log.get("closed").unwrap_or(&Value::Null), &t["scoring_finished"])
    );
    println!(
        "score lanes {} kicks {} rekicks {} starts {} start_failures {} chunks {} lost {} reconciled {} fenced {}",
        s["lanes"], s["kicks"], s["rekicks"], s["starts"], s["start_failures"], s["work_items"], s["lost_attempts"], s["reconciled"], s["fenced_commits"]
    );
    for k in ["cold_ms", "cold_cpu_ms", "chunk_ms", "round_ms", "hosts", "notes", "spend"] {
        println!("{k} {}", ln[k]);
    }
    println!("metered active_ms {} cpu_ms {} do_ms {}", s["active_ms"], s["cpu_ms"], s["do_ms"]);
    for kind in ["summary", "bundle"] {
        let k = &ln["kinds"][kind];
        if !k.is_null() {
            println!("{kind} {} {} kicks {} rekicks {}", k["lanes"], k["by_status"], k["kicks"], k["rekicks"]);
        }
    }
    if let Some(jobs) = c["jobs"].as_array() {
        for j in jobs.iter().filter(|j| j["isolated_crashes"].as_i64().unwrap_or(0) > 0 || !j["artifact_failed"].is_null() || !j["fault"].is_null()) {
            println!("job {} isolated_crashes {} artifact_failed {} artifact_failures {} fault {}", j["job"], j["isolated_crashes"], j["artifact_failed"], j["artifact_failures"], j["fault"]);
        }
    }
}

fn watch_line(pos: &[String]) {
    let [path, label] = pos else { fail("watch-line STATUS.json LABEL") };
    let st = read_json(path);
    let c = cohort(&st, label);
    let ln = &c["lanes"];
    let k = &ln["kinds"]["score"];
    println!(
        "{} fault {} jobs {} receipts {} lanes {} cold {} chunk {} spend {} err {}",
        c["phase"], c["fault"], c["job_status"], c["receipts_recorded"], k["by_status"], ln["cold_ms"], ln["chunk_ms"],
        ln["spend"]["scoring_measured_microusd"], st["last_step_error"]
    );
    let phase = c["phase"].as_str().unwrap_or("");
    if matches!(phase, "closed" | "abandoned" | "transition") || !c["fault"].is_null() {
        exit(3);
    }
}

fn tail_stats(pos: &[String]) {
    let [path] = pos else { fail("tail-stats TAIL.json") };
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
    let mut by: BTreeMap<String, Vec<(u64, u64, String)>> = BTreeMap::new();
    for ev in serde_json::Deserializer::from_str(&text).into_iter::<Value>() {
        let Ok(ev) = ev else { break };
        let url = ev["event"]["request"]["url"].as_str().unwrap_or("");
        let path = url.split_once("://").map_or(url, |x| x.1).split_once('/').map_or("", |x| x.1).split('?').next().unwrap_or("");
        let key = if !path.is_empty() {
            format!("/{path}")
        } else if !ev["event"]["cron"].is_null() {
            "cron".into()
        } else {
            ev["eventType"].as_str().unwrap_or("other").to_owned()
        };
        let outcome = ev["outcome"].as_str().unwrap_or("").to_owned();
        by.entry(key).or_default().push((ev["cpuTime"].as_u64().unwrap_or(0), ev["wallTime"].as_u64().unwrap_or(0), outcome));
    }
    let mut rows: Vec<_> = by.into_iter().collect();
    rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (k, v) in rows {
        let mut cpu: Vec<u64> = v.iter().map(|x| x.0).collect();
        let mut wall: Vec<u64> = v.iter().map(|x| x.1).collect();
        cpu.sort_unstable();
        wall.sort_unstable();
        let q = |a: &[u64], p: f64| a[((a.len() - 1) as f64 * p) as usize];
        let mut outcomes: BTreeMap<&str, usize> = BTreeMap::new();
        for x in &v {
            *outcomes.entry(x.2.as_str()).or_default() += 1;
        }
        println!(
            "{k:28} n={:5} cpu p50={} p95={} max={} wall p50={} p95={} outcomes={outcomes:?}",
            v.len(), q(&cpu, 0.5), q(&cpu, 0.95), cpu[cpu.len() - 1], q(&wall, 0.5), q(&wall, 0.95)
        );
    }
}

fn snapshot_args(pos: &[String]) {
    let [path] = pos else { fail("snapshot-args PLAN.json") };
    let p = read_json(path);
    let owners = std::iter::once(&p["reference"]["owner"]).chain(p["entrants"].as_array().into_iter().flatten().map(|e| &e["owner"]));
    let args: Vec<String> = owners
        .map(|o| format!("--snapshot {}:{}:{}", o["uid"], o["hotkey"].as_str().unwrap_or(""), o["registration_block"]))
        .collect();
    println!("{}", args.join(" "));
}

fn keys_check(pos: &[String]) {
    let [path, pk, rk, instance] = pos else { fail("keys-check KEYS.json PACKAGE_KEY RECEIPT_KEY INSTANCE") };
    let k = read_json(path);
    if k["package_public_key"] != pk.as_str() || k["receipt_public_key"] != rk.as_str() || k["instance"] != instance.as_str() {
        fail(&format!("refused: keys.json differs from the {instance} instance's keys"));
    }
    println!("keys.json names the {instance} instance's keys");
}

fn copy_ties(flags: &BTreeMap<String, String>, pos: &[String]) {
    let [path] = pos else { fail("copy-ties COMPONENTS.json [--require-ties]") };
    let c = read_json(path);
    let jobs = c["jobs"].as_array().unwrap_or_else(|| fail("no jobs"));
    let reference = jobs.iter().find(|j| j["role"] == "reference").unwrap_or_else(|| fail("no reference job"));
    let utility = |j: &Value| j["components"]["utility_v0"].as_str().and_then(|u| u.parse::<i128>().ok());
    let base = utility(reference).unwrap_or_else(|| fail("reference utility"));
    let (mut equal, mut above, mut below, mut max_diff) = (0, 0, 0, 0i128);
    for j in jobs.iter().filter(|j| j["role"] != "reference" && j["package_sha256"] == reference["package_sha256"]) {
        let Some(u) = utility(j) else { continue };
        match u.cmp(&base) {
            std::cmp::Ordering::Equal => equal += 1,
            std::cmp::Ordering::Greater => above += 1,
            std::cmp::Ordering::Less => below += 1,
        }
        max_diff = max_diff.max((u - base).abs());
    }
    println!(
        "{}",
        json!({"copies": equal + above + below, "equal": equal, "above": above, "below": below,
            "max_abs_utility_diff": max_diff.to_string(), "baseline_utility": base.to_string(), "scale": "1e18"})
    );
    if flags.contains_key("require-ties") && above + below > 0 {
        exit(1);
    }
}

fn chunk_keys(pos: &[String]) {
    let [path] = pos else { fail("chunk-keys CHUNK_MAP.json") };
    for o in read_json(path)["objects"].as_array().unwrap_or_else(|| fail("no objects")) {
        let key = o["key"].as_str().unwrap_or_else(|| fail("object key"));
        // cohorts/<label>/<rest>
        println!("{}", key.splitn(3, '/').nth(2).unwrap_or_else(|| fail("object key form")));
    }
}

fn report_summary(pos: &[String]) {
    let [path] = pos else { fail("report-summary VERIFY_REPORT.json") };
    let v = read_json(path);
    let pick: serde_json::Map<String, Value> = ["verified", "label", "receipt_count", "task_count", "chunk_objects", "chunk_bytes"]
        .iter()
        .map(|k| ((*k).to_owned(), v[*k].clone()))
        .collect();
    println!("{}", Value::Object(pick));
    let entries = v["row"]["entries"].as_array().cloned().unwrap_or_default();
    println!("row {} ... {} entries", Value::Array(entries.iter().take(3).cloned().collect()), entries.len());
    if let Some(by) = v["receipts_by_artifact"].as_object() {
        println!("receipts_by_artifact {}", serde_json::to_string(by).unwrap_or_default());
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { fail("a subcommand: load-plan, gate, summary, watch-line, tail-stats, snapshot-args, keys-check, copy-ties, chunk-keys, report-summary") };
    let (flags, pos) = parse(&args[1..]);
    match cmd.as_str() {
        "load-plan" => load_plan(&flags),
        "gate" => gate(&flags, &pos),
        "summary" => summary(&pos),
        "watch-line" => watch_line(&pos),
        "tail-stats" => tail_stats(&pos),
        "snapshot-args" => snapshot_args(&pos),
        "keys-check" => keys_check(&pos),
        "copy-ties" => copy_ties(&flags, &pos),
        "chunk-keys" => chunk_keys(&pos),
        "report-summary" => report_summary(&pos),
        other => fail(&format!("unknown subcommand {other}")),
    }
}

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
//! slop_ninja_eval_ops restart --authority NAME --status STATUS.json --wrangler WRANGLER.toml
//!     --reference ARTIFACT.json --expect-package SHA256 --probe-template PLAN.json --out DIR
//!     --decision REFERENCE [--probe-version V]
//! slop_ninja_eval_ops restart-cohort --restart DIR/restart.json --record RECORD.json --template PLAN.json
//!     --cohort N --label-suffix S --wrangler WRANGLER.toml
//! slop_ninja_eval_ops authority-check --wrangler WRANGLER.toml --status STATUS.json
//!     [--switch [--freeze] [--target-status TARGET.json] | --freeze | --first-deploy]
//! slop_ninja_eval_ops standing-template --restart DIR/restart.json --record RECORD.json
//!     --template TEMPLATE.json --wrangler WRANGLER.toml --tempo-blocks B [--roster-cap N]
//!     [--work-secs S --upload-secs U --upload-bytes BYTES --score-secs L --score-docs D
//!      --score-bytes OBJECT_BYTES --cold-path-secs C [--block-secs T]]
//! slop_ninja_eval_ops probe-cut --status STATUS.json --wrangler WRANGLER.toml --probe-template PLAN.json
//!     --reference ARTIFACT.json [--previous-reference ARTIFACT.json] [--probe-version V] --out DIR
//! slop_ninja_eval_ops template-reinstall --cut DIR/probe-cut.json --record RECORD.json
//!     --template TEMPLATE.json --wrangler WRANGLER.toml --tempo-blocks B [standing-template's options]
//! slop_ninja_eval_ops template-check --template TEMPLATE.json --wrangler WRANGLER.toml
//! ```
//!
//! - `template-check` runs the eval's own install check (`calendar::check`)
//!   on a finished template under the tracked `wrangler.toml` (the exposed
//!   pools, chain and authority it declares) and prints its digest, its
//!   reuse rule and parent pools and its capacity. The store-dependent
//!   install checks (probe versions recorded and fingerprinted, the chain's
//!   references) run only at the install itself.
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
//! - `restart` prepares a chain-record restart under main authority NAME
//!   (`config::Authority`) from the status of the authority it replaces
//!   (`GET /eval/status` before the switch, or the Worker's `/status`) and
//!   the tree's tracked `wrangler.toml`. It refuses unless the status names
//!   the authority the tree tracks (every authority field present), every
//!   cohort there is closed or abandoned, it has no work left and no
//!   successor recorded, NAME is a valid authority name other than an
//!   earlier one's, no earlier label has NAME's prefix, ARTIFACT.json's
//!   `package_sha256` is SHA256 (the R_0 package the restart was decided on,
//!   so a stale reference record never starts the chain), and REFERENCE (a
//!   non-empty line) names the recorded user decision the restart runs under,
//!   since a restart resets the reference chain and after the announcement
//!   needs a new decision (TESTNET.md, "Chain-record restart"). It
//!   writes
//!   `DIR/restart.json`: the authority's vars to commit in
//!   `[env.eval-testnet.vars]` (`EVAL_MAIN_AUTHORITY`,
//!   `EVAL_MAIN_FIRST_COHORT`: one past the replaced authority's highest
//!   cohort number, and `EVAL_MAIN_PREVIOUS`: the earlier named
//!   authorities), the publisher's `RECORD_LABEL_PREFIX` and
//!   `PUBLISH_FROM_EPOCH` (no earlier cohort opened at or after it; also the
//!   coordinator's `restart_target` `from_epoch` and the launch bundle's
//!   issued epoch), the coordinator's `AUTO_TARGET_AUTHORITIES` (NAME, committed
//!   with the three authority vars), the reference, and the probe record's key; and `DIR/probe-plan.json`, the
//!   probe build that creates the restarted chain record: ARTIFACT.json as
//!   R_0 with no predecessor, no entrants, probe version V (default 1), the
//!   authority's first cohort number and label `<prefix>probe-v<V>`, every
//!   other value from PLAN.json. The plan is checked with the coordinator's
//!   own parser and the authority's label and number rule.
//! - `restart-cohort` prints a reward-v1 cohort plan on the restarted chain:
//!   PLAN.json (a v3 plan) with R_0 from `restart.json`, no predecessor, the
//!   probe version in RECORD.json (the probe build's private record, read
//!   back from the eval bucket) at its key and digest, cohort N and label
//!   `<prefix><S>`, and the digest of a named artifact spec frozen with it
//!   (`fanout.score.artifact_spec_sha256`); checked the same way.
//! - `standing-template` prints a standing template for the tracked main
//!   authority (`slop_ninja_eval_cohort_template_v1`): TEMPLATE.json (a
//!   template whose plan is a v3 plan with an admission block naming an
//!   artifact spec) with R_0 from `restart.json`, no predecessor, the probe
//!   version in RECORD.json, the authority's label prefix (without its
//!   trailing `-`), both spec digests and the fewest multipart parts that
//!   carry the spec's largest object at the block's part size, all derived,
//!   and the seeded task order (`reward_v1.task_order = seeded_rank_v1`,
//!   which the restart's probe plan also names; an eval build older than
//!   that field refuses both, so install them only once eval is deployed
//!   with it). The roster cap is N if given, else the largest cap the coordinator's
//!   template check admits (its package step's memory model, the receipt
//!   index, the scoring lanes), clamped to the tracked `ROSTER_CAP` (a
//!   deployment ceiling, not a derived figure); the scoring
//!   budget is the plan's upper bound at that cap (a runaway guard, never a
//!   spend cap). The result passes the coordinator's own template check
//!   under the tracked deployment vars before it is printed. stderr names the
//!   chosen cap, the structural bound at this shape without the ceiling (and
//!   the refusal one past it), the ceiling, the receipts and the validator
//!   bundle schema they need, and the admission window in tempos, blocks and
//!   nominal minutes at B blocks per tempo, with the time between two
//!   windows. B is the chain's tempo interval in blocks: the calendar view's
//!   `admission_window.last_measured.blocks_per_tempo` (a window's blocks
//!   over the epoch boundaries it spanned), never a window's blocks over its
//!   plan's tempos, which a window extended past uncredited tempos inflates.
//!   With `--work-secs`, `--upload-secs`, `--upload-bytes`, `--score-secs`,
//!   `--score-docs`, `--score-bytes` and `--cold-path-secs` (measured on the
//!   live deployment: a cohort's work from its cutoff to its durable
//!   package; one submission of BYTES bytes from its commitment to its
//!   `verified` answer; one scoring lane of D documents with an object of
//!   OBJECT_BYTES bytes, from the scoring start to its end; the longest
//!   cold path of a lane loading that object) the plan's `admission_tempos` is derived
//!   instead of taken from TEMPLATE.json (`calendar::derive_admission_tempos`:
//!   the longest window that still lets a full cohort close K tempos after
//!   it opened, with the work bound, which includes one entrant's scoring
//!   lane of the largest object and the verdict queue of a full roster
//!   (`--roster-cap`, else the deployment's ceiling) still verifying at the
//!   cutoff, at most half of its tempos and the largest
//!   object's submission at most half of the window). Seconds per tempo are
//!   B blocks at `--block-secs` seconds (default the nominal 12, an
//!   assumption); stderr gives the derivation and the interval it used.
//! - `probe-cut` prepares a same-authority probe cut
//!   (`eval::probe_cut::prepare`): a new probe version for the tracked
//!   authority while its cohorts are open, with no chain-record restart.
//!   STATUS.json is the authority's eval status read just before; the
//!   probe build PLAN.json supplies every other value; ARTIFACT.json (and
//!   `--previous-reference` exactly when the chain has a predecessor) are
//!   the chain's current references as the stored records name them. The
//!   version defaults to one past every probe version built there; the
//!   cohort number is one past the highest held (so the first cohort the
//!   reinstalled template governs is numbered one past the cut), the label
//!   `<prefix>probe-v<V>`. It writes `DIR/probe-plan.json`
//!   (`reward_v1.probe_cut`, the seeded task order, no admission block,
//!   checked with the coordinator's parser) and `DIR/probe-cut.json` (the
//!   version, the record key, the references and the open cohorts it runs
//!   beside). Queue the plan with `POST /eval/plan`; the eval Worker checks
//!   the cut again in its queueing transaction and refuses one over a
//!   moved chain, a pending promotion, another open cut or a version
//!   already built.
//! - `template-reinstall` prints the standing template naming that cut's
//!   probe version: `standing-template` with `--cut` in place of
//!   `--restart` (the cut's reference, the version, size and key of the cut
//!   record and the SHA-256 of RECORD.json, the record read back from the
//!   eval bucket once the cut closed). Install it with `POST /eval/template`
//!   before the next cohort queues; it governs the cohorts queued after the
//!   install, never a queued or running one.
//! - `authority-check` is the pre-deploy refusal for the eval Worker
//!   (`config::deploy_check`): the deployed authority (`authority_name`,
//!   `first_cohort`, `predecessors`, `cohorts`, `superseded_by`,
//!   `succession_recorded` and `work` in STATUS.json, which must be an eval
//!   status) must equal the one the tree to deploy tracks
//!   (`EVAL_MAIN_AUTHORITY`, `EVAL_MAIN_FIRST_COHORT`, `EVAL_MAIN_PREVIOUS`)
//!   and must not be superseded, or exit 1; `--freeze` alone redeploys a
//!   superseded one frozen. It fails closed: an error reply, a Worker status
//!   without an eval status, or a status without any of those fields
//!   refuses. With `--switch` (the switch or rollback commit only) the tree's
//!   authority must instead follow the deployed one (its predecessors are
//!   the deployed one's, then the deployed one), which must be idle with no
//!   work left, with its standing template paused, every label and number
//!   outside the tree's authority, and no other successor recorded; or be
//!   one of the deployed one's predecessors (a rollback), which needs
//!   TARGET.json, that authority's own status (`GET
//!   /eval/authority/<name>/status`), and is refused once succession
//!   happened (it recorded a successor, or the deployed one recorded a
//!   succession or lists a cohort) unless `--freeze`. With `--first-deploy`
//!   (the one deploy of authority support only) the status must be an eval
//!   status of a build that reports no authority, and the tree must track
//!   the original. Every mode also refuses a tree whose coordinator
//!   `AUTO_TARGET_AUTHORITIES` (`[env.testnet.vars]` of the same file) does
//!   not name the tracked authority (a named one): the automatic handoff
//!   would leave its cohorts outside the record. The vars are never passed
//!   with `--var`.
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use slop_ninja_coordinator::config::{Authority, DeployMode, DeployedAuthority, deploy_check};
use slop_ninja_coordinator::eval::plan::{Ceilings, CohortPlan, LoadPlan, PLAN_SCHEMA_V3, PROBE_PLAN_SCHEMA};
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
            Some(k @ ("require-ties" | "switch" | "freeze" | "first-deploy")) => {
                flags.insert(k.into(), String::new());
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

/// The `[env.eval-testnet.vars]` table of a tracked `wrangler.toml`: each
/// `KEY = "VALUE"` line of that section (the form the file uses).
fn eval_vars(path: &str) -> BTreeMap<String, String> {
    let vars = section_vars(path, "[env.eval-testnet.vars]");
    if vars.is_empty() {
        fail(&format!("{path}: no [env.eval-testnet.vars] section"));
    }
    vars
}

/// The `KEY = "VALUE"` lines of one `[...]` section of a tracked
/// `wrangler.toml` (empty when the section is absent).
fn section_vars(path: &str, section: &str) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("read {path}: {e}")));
    let mut vars = BTreeMap::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == section;
            continue;
        }
        if !inside || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
            vars.insert(k.trim().to_owned(), v.to_owned());
        }
    }
    vars
}

/// The coordinator's automatic handoff hands in only the cohorts of the
/// authorities its tracked `AUTO_TARGET_AUTHORITIES` names (`[env.testnet.vars]`
/// of the same `wrangler.toml`, `signing::auto::Record`). A tree whose eval
/// authority is not among them would leave every cohort of that authority
/// `outside` the record, so the coordinator would never install its
/// targets: refused. The original authority has no label prefix to bind.
fn auto_target_check(path: &str, tracked: &Authority) -> Result<(), String> {
    let Some(prefix) = tracked.label_prefix() else {
        return Ok(());
    };
    let coord = section_vars(path, "[env.testnet.vars]");
    let named = coord.get("AUTO_TARGET_AUTHORITIES").map(String::as_str);
    let record = slop_ninja_coordinator::signing::auto::Record::parse(named)
        .map_err(|code| format!("the tree's AUTO_TARGET_AUTHORITIES does not name a record ({code})"))?;
    if !record.admits(&format!("{prefix}0")) {
        return Err(format!(
            "the tree's AUTO_TARGET_AUTHORITIES ({}) does not name the tracked eval authority {}: the coordinator would leave its cohorts outside the record (change both in the same commit)",
            named.unwrap_or_default(),
            tracked.name()
        ));
    }
    Ok(())
}

/// The authority a tracked `wrangler.toml` names for the main instance.
fn tracked_authority(vars: &BTreeMap<String, String>) -> Authority {
    let v = |k: &str| vars.get(k).map(String::as_str);
    Authority::parse_with(v("EVAL_MAIN_AUTHORITY"), v("EVAL_MAIN_FIRST_COHORT"), v("EVAL_MAIN_PREVIOUS"))
        .unwrap_or_else(|_| fail("wrangler.toml: the EVAL_MAIN_* vars name no authority"))
}

/// The main instance's plan ceilings as the tracked `wrangler.toml` sets them
/// (`ROSTER_CAP`, reward-v1 only), so the plan check matches the deployment.
fn main_ceilings(vars: &BTreeMap<String, String>) -> Ceilings {
    let roster_cap =
        vars.get("ROSTER_CAP").map(|v| v.parse::<usize>().unwrap_or_else(|_| fail("wrangler.toml: ROSTER_CAP")));
    Ceilings { roster_cap, accept_reward_v0: false }
}

fn authority_check(flags: &BTreeMap<String, String>) {
    let tracked = tracked_authority(&eval_vars(&flag::<String>(flags, "wrangler")));
    let status = read_json(&flag::<String>(flags, "status"));
    let target = flags.get("target-status").map(String::as_str).map(read_json);
    let mode = match (flags.contains_key("switch"), flags.contains_key("freeze"), flags.contains_key("first-deploy")) {
        (false, false, false) => DeployMode::Same,
        (false, true, false) => DeployMode::Frozen,
        (true, freeze, false) => DeployMode::Switch { freeze },
        (false, false, true) => DeployMode::FirstDeploy,
        _ => fail("--first-deploy takes neither --switch nor --freeze"),
    };
    let wrangler: String = flag(flags, "wrangler");
    // P1-F preflight: succession reads the predecessor's open work under its
    // current schema, so a switch build must not raise the deployed
    // authority's schema; ship the migration under the current authority
    // first, then switch.
    if matches!(mode, DeployMode::Switch { .. })
        && let Err(why) = switch_schema_check(&status, slop_ninja_coordinator::sql::EVAL_COHORT_SCHEMA.latest())
    {
        println!("refused: {why}");
        exit(1);
    }
    match deploy_check(&tracked, &status, target.as_ref(), mode).and_then(|line| auto_target_check(&wrangler, &tracked).map(|()| line)) {
        Ok(line) => println!("{line}"),
        Err(why) => {
            println!("refused: {why}");
            exit(1);
        }
    }
}

/// P1-F `switch_needs_migration`: a switch build's eval schema must equal
/// the deployed authority's reported `schema_version`; a status that reports
/// none is refused too (its version is unknown).
fn switch_schema_check(status: &Value, build: u32) -> Result<(), String> {
    match status.get("schema_version").and_then(Value::as_u64) {
        Some(v) if v == u64::from(build) => Ok(()),
        Some(v) => Err(format!(
            "switch_needs_migration: the deployed authority's store is at schema {v} and this build migrates to {build}; deploy the migration under the current authority first, then switch"
        )),
        None => Err("switch_needs_migration: the status reports no schema_version".into()),
    }
}

/// A plan the restarted authority takes: the coordinator's own parser, and
/// the authority's label and cohort number rule.
fn checked_plan(plan: &Value, authority: &Authority, ceilings: Ceilings) -> Vec<u8> {
    let bytes = serde_json::to_vec_pretty(plan).expect("plan json");
    let parsed = CohortPlan::parse(&bytes, ceilings)
        .unwrap_or_else(|e| fail(&format!("the coordinator refuses this plan: {}", e.0)));
    authority
        .admits(&parsed.label, parsed.cohort)
        .unwrap_or_else(|e| fail(&format!("the authority refuses this plan: {e}")));
    bytes
}

fn restart(flags: &BTreeMap<String, String>) {
    let name: String = flag(flags, "authority");
    let vars = eval_vars(&flag::<String>(flags, "wrangler"));
    let ceilings = main_ceilings(&vars);
    let tracked = tracked_authority(&vars);
    let st = read_json(&flag::<String>(flags, "status"));
    // Every authority field or a refusal: a status this cannot read is never
    // taken for the original's.
    let deployed = DeployedAuthority::from_status(&st).unwrap_or_else(|e| fail(&format!("status: {e}")));
    let replaced = deployed.name.clone();
    // `GET /eval/status`, or the Worker's `/status` that carries it.
    let st = if st["eval_cohort"].is_object() { st["eval_cohort"].clone() } else { st };
    let cohorts = st["cohorts"].as_array().unwrap_or_else(|| fail("status: no cohorts"));
    if replaced != tracked.name() {
        fail(&format!("the status names {replaced}, the tree tracks {}", tracked.name()));
    }
    if let Some(by) = &deployed.superseded_by {
        fail(&format!("{replaced} is already superseded by {by}"));
    }
    if deployed.work {
        fail(&format!("{replaced} still has work (a closure, a spend settlement, a purge, an admission clean-up or admitted objects awaiting the publisher's report)"));
    }
    // The recorded decision the restart runs under (kept in restart.json).
    let decision: String = flag(flags, "decision");
    if decision.trim().is_empty() || decision.contains(['\n', '\r']) {
        fail("--decision names the recorded decision on one non-empty line");
    }
    let tracked_preds: Vec<String> = tracked.predecessors().into_iter().map(str::to_owned).collect();
    if deployed.first_cohort != tracked.first_cohort() || deployed.predecessors != tracked_preds {
        fail("the status's first cohort or predecessors differ from the tree's");
    }
    // The R_0 package the restart was decided on: a stale reference record
    // (another package of the same weights, or the earlier R_0) is refused.
    let reference = read_json(&flag::<String>(flags, "reference"));
    let expect: String = flag(flags, "expect-package");
    if reference["package_sha256"].as_str() != Some(expect.as_str()) {
        fail("the reference record's package_sha256 is not --expect-package");
    }
    // The new authority's predecessors: the replaced one's, then the
    // replaced one itself when it is named (the original is always first).
    let mut previous: Vec<String> =
        tracked.predecessors().into_iter().filter(|p| *p != "main").map(str::to_owned).collect();
    if !tracked.is_original() {
        previous.push(replaced.clone());
    }
    if name == replaced || previous.contains(&name) || name == "main" {
        fail(&format!("{name} is the authority being replaced or an earlier one"));
    }
    let mut earlier = Vec::new();
    let mut busy = Vec::new();
    let mut opened_max = 0u64;
    for c in cohorts {
        let label = c["label"].as_str().unwrap_or_else(|| fail("status: a cohort without a label"));
        let number = c["cohort"].as_u64().unwrap_or_else(|| fail("status: a cohort without a number"));
        let phase = c["phase"].as_str().unwrap_or("");
        if !matches!(phase, "closed" | "abandoned") {
            busy.push(format!("{label} is {phase}"));
        }
        opened_max = opened_max.max(c["opened_epoch"].as_u64().unwrap_or(0));
        earlier.push((label.to_owned(), number));
    }
    if !busy.is_empty() {
        fail(&format!("the authority being replaced is not idle: {}", busy.join("; ")));
    }
    // Every number of every earlier authority is below the replaced one's
    // first cohort, so one past the replaced one's highest is past them all.
    let first = earlier.iter().map(|(_, n)| *n).max().unwrap_or(0).max(tracked.first_cohort().saturating_sub(1)) + 1;
    let previous_refs: Vec<&str> = previous.iter().map(String::as_str).collect();
    let authority = Authority::named_after(&name, first, &previous_refs)
        .unwrap_or_else(|_| fail(&format!("{name} cannot name an authority")));
    authority.follows(earlier.iter().map(|(l, n)| (l.as_str(), *n))).unwrap_or_else(|e| fail(e));
    let prefix = authority.label_prefix().expect("a named authority's prefix");
    // No earlier cohort opened at or after it, and no restarted cohort opens
    // before it (each opens at a boundary after the switch).
    let observed = st["observed"]["epoch"]["index"].as_u64().unwrap_or_else(|| fail("status: no observed epoch"));
    let from_epoch = observed.max(opened_max + 1);
    let version: u32 = opt(flags, "probe-version").unwrap_or(1);
    let mut plan = read_json(&flag::<String>(flags, "probe-template"));
    plan["schema"] = json!(PROBE_PLAN_SCHEMA);
    plan["cohort"] = json!(first);
    plan["label"] = json!(format!("{prefix}probe-v{version}"));
    plan["reference"] = reference.clone();
    plan["previous_reference"] = Value::Null;
    plan["entrants"] = json!([]);
    plan["allow_list"] = json!([]);
    let v = plan["reward_v1"].as_object_mut().unwrap_or_else(|| fail("probe template: no reward_v1 block"));
    v.insert("probe_version".into(), json!(version));
    v.insert("probes".into(), json!([]));
    // A probe version cut now seals in the seeded task order (finding L4).
    v.insert("task_order".into(), json!(slop_ninja_coordinator::eval::plan::TASK_ORDER_SEEDED_RANK));
    v["canary"]["drill"] = json!([]);
    let bytes = checked_plan(&plan, &authority, ceilings);
    let dir = std::path::PathBuf::from(flag::<String>(flags, "out"));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(&format!("{}: {e}", dir.display())));
    std::fs::write(dir.join("probe-plan.json"), &bytes).unwrap_or_else(|e| fail(&format!("probe plan: {e}")));
    let record_key = format!("{}/record.json", authority.probe_prefix(version));
    let out = json!({
        "schema": "slop_ninja_eval_restart_v1",
        "decision": decision.trim(),
        "authority": name,
        "replaces": replaced,
        "first_cohort": first,
        "label_prefix": prefix,
        // Commit these in [env.eval-testnet.vars] (never --var).
        "vars": {"EVAL_MAIN_AUTHORITY": name, "EVAL_MAIN_FIRST_COHORT": first.to_string(),
            "EVAL_MAIN_PREVIOUS": previous.join(",")},
        // Commit this in the coordinator's [env.testnet.vars], in the same
        // commit as the three above: the automatic handoff hands in only the
        // named authorities' cohorts (`authority-check` refuses a tree whose
        // eval authority it does not name).
        "coordinator": {"AUTO_TARGET_AUTHORITIES": name},
        // Commit these in the publisher's [env.testnet.vars].
        "publisher": {"RECORD_LABEL_PREFIX": prefix, "PUBLISH_FROM_EPOCH": from_epoch.to_string()},
        // The coordinator's restart_target from_epoch and the launch bundle's issued epoch.
        "record_from_epoch": from_epoch,
        "reference": reference,
        "probe_version": version,
        "probe_record_key": record_key,
        "probe_plan_sha256": sha_hex(&bytes),
        "replaced_authority": {"cohorts": earlier.len(), "highest_cohort": first - 1, "observed_epoch": observed,
            "reference": st["reward"]["references"].clone()},
    });
    let text = serde_json::to_string_pretty(&out).expect("restart json");
    std::fs::write(dir.join("restart.json"), &text).unwrap_or_else(|e| fail(&format!("restart.json: {e}")));
    println!("{text}");
}

fn restart_cohort(flags: &BTreeMap<String, String>) {
    let r = read_json(&flag::<String>(flags, "restart"));
    let name = r["authority"].as_str().unwrap_or_else(|| fail("restart.json: authority"));
    let first = r["first_cohort"].as_u64().unwrap_or_else(|| fail("restart.json: first_cohort"));
    let authority = Authority::named(name, first).unwrap_or_else(|_| fail("restart.json: authority"));
    let ceilings = main_ceilings(&eval_vars(&flag::<String>(flags, "wrangler")));
    let record_bytes = std::fs::read(flag::<String>(flags, "record")).unwrap_or_else(|e| fail(&format!("record: {e}")));
    let record: Value = serde_json::from_slice(&record_bytes).unwrap_or_else(|e| fail(&format!("record: {e}")));
    let version = record["version"].as_u64().unwrap_or_else(|| fail("record: version"));
    if Some(version) != r["probe_version"].as_u64() {
        fail("the record is not the restart's probe version");
    }
    let mut plan = read_json(&flag::<String>(flags, "template"));
    if plan["schema"] != PLAN_SCHEMA_V3 {
        fail("template: not a reward-v1 cohort plan");
    }
    let suffix: String = flag(flags, "label-suffix");
    plan["cohort"] = json!(flag::<u64>(flags, "cohort"));
    plan["label"] = json!(format!("{}{suffix}", authority.label_prefix().expect("prefix")));
    plan["reference"] = r["reference"].clone();
    plan["previous_reference"] = Value::Null;
    let v = plan["reward_v1"].as_object_mut().unwrap_or_else(|| fail("template: no reward_v1 block"));
    v.insert(
        "probes".into(),
        json!([{"version": version, "size": record["size"], "key": r["probe_record_key"], "sha256": sha_hex(&record_bytes)}]),
    );
    v.remove("probe_version");
    // A named artifact spec is frozen with this build's digest of it in
    // `fanout.score.artifact_spec_sha256` (beside an admission block's, which
    // must equal it), so the cohort binds the v2 limits record that names it.
    if let Some(id) = plan["fanout"]["score"]["artifact_spec"].as_str().map(str::to_owned)
        && plan["fanout"]["score"]["artifact_spec_sha256"].is_null()
    {
        let sha = slop_ninja_coordinator::eval::plan::load_spec_sha256_of(&id)
            .unwrap_or_else(|| fail(&format!("template: {id} is not a spec the scorer admits loads by")));
        plan["fanout"]["score"]["artifact_spec_sha256"] = json!(sha);
    }
    let bytes = checked_plan(&plan, &authority, ceilings);
    println!("{}", String::from_utf8(bytes).expect("utf-8"));
}

/// The main instance's evaluation config as a tracked `wrangler.toml`
/// declares it, for the coordinator's own plan and template checks. The
/// private seed is never needed by a check (zeros here).
fn tracked_eval_config(vars: &BTreeMap<String, String>) -> slop_ninja_coordinator::eval::engine::EvalConfig {
    let v = |k: &str| vars.get(k).cloned().unwrap_or_else(|| fail(&format!("wrangler.toml: {k}")));
    let genesis: [u8; 32] = hex::decode(v("CHAIN_GENESIS").trim_start_matches("0x"))
        .ok()
        .and_then(|b| b.try_into().ok())
        .unwrap_or_else(|| fail("wrangler.toml: CHAIN_GENESIS"));
    slop_ninja_coordinator::eval::engine::EvalConfig {
        chain: slop_ninja_protocol::ChainBinding {
            genesis: slop_ninja_protocol::Digest(genesis),
            netuid: v("SUBNET_NETUID").parse().unwrap_or_else(|_| fail("wrangler.toml: SUBNET_NETUID")),
            mechanism: 0,
        },
        private_seed: [0; 32],
        instance: slop_ninja_coordinator::eval::engine::INSTANCE_MAIN.into(),
        max_score_lanes: v("MAX_SCORE_LANES").parse().unwrap_or_else(|_| fail("wrangler.toml: MAX_SCORE_LANES")),
        ceilings: main_ceilings(vars),
        exposed_pools: v("EXPOSED_POOLS").split(',').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect(),
        scorer_binary: v("SCORER_BINARY_SHA256"),
        authority: tracked_authority(vars),
    }
}

fn standing_template(flags: &BTreeMap<String, String>) {
    let r = read_json(&flag::<String>(flags, "restart"));
    let vars = eval_vars(&flag::<String>(flags, "wrangler"));
    let cfg = tracked_eval_config(&vars);
    if r["authority"].as_str() != Some(cfg.authority.name()) || r["first_cohort"].as_u64() != Some(cfg.authority.first_cohort()) {
        fail("restart.json names another authority than the tree tracks");
    }
    let record_bytes = std::fs::read(flag::<String>(flags, "record")).unwrap_or_else(|e| fail(&format!("record: {e}")));
    let record: Value = serde_json::from_slice(&record_bytes).unwrap_or_else(|e| fail(&format!("record: {e}")));
    let version = record["version"].as_u64().unwrap_or_else(|| fail("record: version"));
    if Some(version) != r["probe_version"].as_u64() {
        fail("the record is not the restart's probe version");
    }
    let probe = json!({"version": version, "size": record["size"], "key": r["probe_record_key"], "sha256": sha_hex(&record_bytes)});
    template_out(flags, &cfg, "standing-template", r["reference"].clone(), probe);
}

/// `template-reinstall`: the standing template for the tracked authority
/// naming a same-authority probe cut's version (no restart).
fn template_reinstall(flags: &BTreeMap<String, String>) {
    let cut = read_json(&flag::<String>(flags, "cut"));
    let vars = eval_vars(&flag::<String>(flags, "wrangler"));
    let cfg = tracked_eval_config(&vars);
    if cut["authority"].as_str() != Some(cfg.authority.name()) || cut["first_cohort"].as_u64() != Some(cfg.authority.first_cohort()) {
        fail("the probe cut names another authority than the tree tracks");
    }
    let record_bytes = std::fs::read(flag::<String>(flags, "record")).unwrap_or_else(|e| fail(&format!("record: {e}")));
    let probe = slop_ninja_coordinator::eval::probe_cut::template_probe(&cut, &record_bytes).unwrap_or_else(|e| fail(&e));
    // A reinstalled template declares its generation pause bound (its
    // cohorts' seals bind it); only templates stored before the bound was
    // required take the compiled one.
    let t = read_json(&flag::<String>(flags, "template"));
    if !t["plan"]["fanout"]["generation"]["failures_before_pause"].is_u64() {
        fail("template: plan.fanout.generation.failures_before_pause (a reinstalled template declares it)");
    }
    template_out(flags, &cfg, "template-reinstall", cut["reference"].clone(), probe);
}

/// `template-check`: the eval's install check on a finished template under
/// the tracked configuration; refuses with the eval's own code.
fn template_check(flags: &BTreeMap<String, String>) {
    use slop_ninja_coordinator::eval::calendar;
    let vars = eval_vars(&flag::<String>(flags, "wrangler"));
    let cfg = tracked_eval_config(&vars);
    let path = flag::<String>(flags, "template");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| fail(&format!("template: {e}")));
    let t = calendar::check(&cfg, &bytes).unwrap_or_else(|e| fail(&format!("template refused: {}", e.code())));
    let v = t.plan.reward_v1.as_ref().unwrap_or_else(|| fail("template: no reward_v1 block"));
    let out = json!({
        "sha256": sha_hex(&bytes),
        "label_prefix": t.label_prefix,
        "pool": t.plan.pool.pool_id,
        "parent_pools": v.parent_pools.iter().map(|p| json!({"pool_id": p.pool_id, "manifest_key": p.manifest_key,
            "manifest_sha256": p.manifest_sha256})).collect::<Vec<_>>(),
        "reuse": {"max_uses": v.reuse.max_uses, "min_gap": v.reuse.min_gap,
            "exposed": v.reuse.exposed.map(slop_ninja_coordinator::eval::plan::ExposedReuse::name)},
        "spares_permille": v.spares_permille,
        "capacity": calendar::capacity(&t),
    });
    println!("{}", serde_json::to_string_pretty(&out).expect("json"));
}

/// `probe-cut`: a same-authority probe cut's plan and record.
fn probe_cut(flags: &BTreeMap<String, String>) {
    let vars = eval_vars(&flag::<String>(flags, "wrangler"));
    let tracked = tracked_authority(&vars);
    let status = read_json(&flag::<String>(flags, "status"));
    let inputs = slop_ninja_coordinator::eval::probe_cut::CutInputs {
        status: &status,
        tracked: &tracked,
        ceilings: main_ceilings(&vars),
        probe_template: read_json(&flag::<String>(flags, "probe-template")),
        reference: read_json(&flag::<String>(flags, "reference")),
        previous_reference: flags.get("previous-reference").map(|p| read_json(p)),
        probe_version: opt(flags, "probe-version"),
    };
    let cut = slop_ninja_coordinator::eval::probe_cut::prepare(inputs).unwrap_or_else(|e| fail(&e));
    let dir = std::path::PathBuf::from(flag::<String>(flags, "out"));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(&format!("{}: {e}", dir.display())));
    std::fs::write(dir.join("probe-plan.json"), &cut.plan).unwrap_or_else(|e| fail(&format!("probe plan: {e}")));
    let text = serde_json::to_string_pretty(&cut.record).expect("cut json");
    std::fs::write(dir.join("probe-cut.json"), &text).unwrap_or_else(|e| fail(&format!("probe-cut.json: {e}")));
    println!("{text}");
}

/// The standing template TEMPLATE.json for the tracked authority with
/// `reference` and the one probe version `probe`, every bound derived and
/// checked as `standing-template` documents; printed to stdout.
fn template_out(
    flags: &BTreeMap<String, String>,
    cfg: &slop_ninja_coordinator::eval::engine::EvalConfig,
    tool: &str,
    reference: Value,
    probe: Value,
) {
    use slop_ninja_coordinator::eval::{calendar, engine};
    let prefix = cfg.authority.label_prefix().unwrap_or_else(|| fail("the tracked authority is the original (no label prefix)"));
    let mut t = read_json(&flag::<String>(flags, "template"));
    if t["schema"] != calendar::TEMPLATE_SCHEMA || t["plan"]["schema"] != PLAN_SCHEMA_V3 {
        fail("template: not a standing template of a reward-v1 plan");
    }
    t["label_prefix"] = json!(prefix.trim_end_matches('-'));
    let plan = &mut t["plan"];
    plan["cohort"] = json!(cfg.authority.first_cohort());
    plan["label"] = json!(format!("{prefix}template"));
    plan["entrants"] = json!([]);
    plan["reference"] = reference;
    plan["previous_reference"] = Value::Null;
    let v = plan["reward_v1"].as_object_mut().unwrap_or_else(|| fail("template: no reward_v1 block"));
    v.insert("probes".into(), json!([probe]));
    v.remove("probe_version");
    v.remove("probe_cut");
    // Every cohort the template opens seals in the seeded task order (finding
    // L4); the coordinator's template check requires it.
    v.insert("task_order".into(), json!(slop_ninja_coordinator::eval::plan::TASK_ORDER_SEEDED_RANK));
    let id = plan["admission"]["artifact_spec"].as_str().map(str::to_owned).unwrap_or_else(|| fail("template: admission.artifact_spec"));
    let bounds = slop_ninja_coordinator::eval::admission::SpecBounds::of_id(&id)
        .unwrap_or_else(|| fail(&format!("template: {id} is not a spec the scorer admits by")));
    plan["admission"]["artifact_spec_sha256"] = json!(bounds.spec_sha256);
    // Admission v2: every object is one presigned PUT under the compiled
    // upload URL policy; the v1 part fields go.
    let adm = plan["admission"].as_object_mut().unwrap_or_else(|| fail("template: admission"));
    adm.remove("part_size");
    adm.remove("max_parts");
    adm.insert(
        "upload_policy".into(),
        serde_json::to_value(slop_ninja_coordinator::eval::plan::UploadPolicyPlan::v1()).unwrap_or_else(|_| fail("upload policy")),
    );
    plan["fanout"]["score"]["artifact_spec"] = json!(id);
    plan["fanout"]["score"]["artifact_spec_sha256"] = json!(bounds.spec_sha256);
    plan["fanout"]["score"].as_object_mut().unwrap_or_else(|| fail("template: fanout.score")).remove("artifact_max_mib");
    // The admission window: derived from measurements when they are given,
    // else the template's own, shown at the measured blocks per tempo.
    let tempo_blocks: u64 = flag(flags, "tempo-blocks");
    let measured = ["work-secs", "upload-secs", "upload-bytes", "score-secs", "score-docs", "score-bytes", "cold-path-secs"]
        .map(|f| opt::<u64>(flags, f));
    // The roster the verdict queue at the cutoff is counted for: the cap
    // asked for, else the deployment's ceiling (the cap chosen below is at
    // most it), else the protocol's bound.
    let queue_roster = opt::<u64>(flags, "roster-cap").unwrap_or_else(|| {
        cfg.ceilings.roster_cap.map_or(u64::from(slop_ninja_protocol::MAX_ROSTER), |c| c as u64)
    });
    let block_secs = opt::<u64>(flags, "block-secs").unwrap_or(calendar::NOMINAL_BLOCK_SECS);
    let derivation = match measured {
        [
            Some(work_secs),
            Some(upload_secs),
            Some(upload_bytes),
            Some(score_secs),
            Some(score_docs),
            Some(score_bytes),
            Some(cold_path_secs),
        ] => {
            let typed: CohortPlan = serde_json::from_value(plan.clone()).unwrap_or_else(|e| fail(&format!("template plan: {e}")));
            let inputs = calendar::WindowInputs {
                tempo_blocks,
                block_secs,
                work_secs,
                upload_secs,
                upload_bytes,
                score_secs,
                score_docs,
                score_bytes,
                cold_path_secs,
            };
            let d = calendar::derive_admission_tempos(&typed, bounds.object_bytes, queue_roster, &inputs)
                .unwrap_or_else(|e| fail(&format!("admission window: {e}")));
            plan["admission_tempos"] = json!(d.admission_tempos);
            Some((d, typed.entrant_docs()))
        }
        [None, None, None, None, None, None, None] => None,
        _ => fail(
            "--work-secs, --upload-secs, --upload-bytes, --score-secs, --score-docs, --score-bytes and --cold-path-secs go together",
        ),
    };
    // The scoring budget at a cap: the plan's own upper bound there, checked
    // under `cfg` (the tracked ceilings, or none for the structural bound).
    let budgeted_in = |cfg: &engine::EvalConfig, t: &Value, cap: u64| -> Result<Value, String> {
        let mut t = t.clone();
        t["plan"]["admission"]["roster_cap"] = json!(cap);
        t["plan"]["fanout"]["score"]["scoring_budget_microusd"] = json!(u64::MAX / 4);
        let mut full = t["plan"].clone();
        full["label"] = json!(format!("{prefix}{}", u64::MAX));
        full["entrants"] = json!((0..cap).map(|n| {
            let mut a = full["reference"].clone();
            a["owner"] = json!({"uid": n + 1, "hotkey": format!("0x{}", sha_hex(&n.to_le_bytes())), "registration_block": 1});
            a["package_sha256"] = json!(sha_hex(format!("standing-template/{n}").as_bytes()));
            a["opening_nonce"] = json!(sha_hex(&n.to_be_bytes()));
            a["extrinsic_index"] = json!(n);
            a["commitment_block"] = json!(2);
            a["object_key"] = json!(format!("admission/template-check/{n}.object"));
            a
        }).collect::<Vec<_>>());
        if full["previous_reference"].is_null() {
            full["previous_reference"] = full["reference"].clone();
            let sha = sha_hex(b"standing-template/predecessor");
            full["previous_reference"]["manifest_key"] = json!(format!("artifacts/packages/{sha}.json"));
            full["previous_reference"]["package_sha256"] = json!(sha);
        }
        let (_, adm) = engine::admit(cfg, &serde_json::to_vec(&full).map_err(|e| e.to_string())?).map_err(|e| format!("plan at cap {cap}: {e:?}"))?;
        t["plan"]["fanout"]["score"]["scoring_budget_microusd"] = json!(adm.scoring_upper_bound_microusd);
        let bytes = serde_json::to_vec(&t).map_err(|e| e.to_string())?;
        calendar::check(cfg, &bytes).map(|_| t).map_err(|e| format!("template at cap {cap}: {e:?}"))
    };
    let budgeted = |t: &Value, cap: u64| budgeted_in(cfg, t, cap);
    // The structural bound at this shape: the largest cap the coordinator's
    // own checks admit (memory model, receipt index, lanes) with no
    // deployment ceiling, and the refusal one past it names. Every bound
    // grows with the roster, so the admitted caps are 1..=bound.
    let structural = {
        let mut open = cfg.clone();
        open.ceilings.roster_cap = None;
        let (mut lo, mut hi) = (0u64, u64::from(slop_ninja_protocol::MAX_ROSTER));
        if budgeted_in(&open, &t, hi).is_ok() {
            lo = hi;
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if budgeted_in(&open, &t, mid).is_ok() { lo = mid } else { hi = mid }
        }
        let past = (lo < u64::from(slop_ninja_protocol::MAX_ROSTER)).then(|| budgeted_in(&open, &t, lo + 1).err()).flatten();
        (lo, past)
    };
    let (cap, out) = match opt::<u64>(flags, "roster-cap") {
        Some(n) => (n, budgeted(&t, n).unwrap_or_else(|e| fail(&format!("the coordinator's template check refuses roster cap {n}: {e}")))),
        None => {
            let ceiling = cfg.ceilings.roster_cap.map_or(u64::from(slop_ninja_protocol::MAX_ROSTER), |c| c as u64);
            (1..=ceiling)
                .rev()
                .find_map(|n| budgeted(&t, n).ok().map(|t| (n, t)))
                .unwrap_or_else(|| fail(&format!("the coordinator's template check refuses every roster cap ({})", budgeted(&t, 1).err().unwrap_or_default())))
        }
    };
    if derivation.is_some() && cap > queue_roster {
        fail(&format!("roster cap {cap} exceeds the roster {queue_roster} the verdict queue was counted for"));
    }
    let bytes = serde_json::to_vec_pretty(&out).expect("template json");
    let parsed = calendar::check(cfg, &bytes).unwrap_or_else(|e| fail(&format!("template check: {e:?}")));
    // 40 is a deployment ceiling, not a derived figure: ROSTER_CAP in the
    // tracked wrangler.toml bounds every plan's roster cap; each cohort
    // freezes its own cap in its plan, and widening the ceiling is a deploy
    // that changes ROSTER_CAP (up to the structural bound printed here).
    eprintln!(
        "{tool}: roster_cap {cap}; structural bound {} at this shape (memory model, receipt index, lanes; one more: {}); deployment ceiling ROSTER_CAP {}; {}",
        structural.0,
        structural.1.as_deref().unwrap_or("MAX_ROSTER"),
        cfg.ceilings.roster_cap.map_or("none".to_owned(), |c| c.to_string()),
        calendar::capacity(&parsed)
    );
    let (k, a) = (parsed.plan.k_tempos, parsed.plan.admission_tempos);
    eprintln!(
        "{tool}: admission window {} per cohort of {} (K {k} tempos at {tempo_blocks} blocks); no window for {} between windows (work, publication, transition {} tempos); a commitment before a window opens is refused",
        calendar::window_json(a, tempo_blocks),
        calendar::window_json(k, tempo_blocks),
        calendar::window_json(k.saturating_sub(a), tempo_blocks),
        parsed.plan.transition_tempos,
    );
    if let Some((d, entrant_docs)) = derivation {
        eprintln!(
            "{tool}: admission_tempos {} derived = K {k} - transition {} - work tempos {} (work bound {} s = measured work {} s + largest submission {} s ({} bytes, scaled from the measured one) + verdict queue {} s ({} round(s) of {} verdicts for a roster of {queue_roster}, each {} s: the measured cold path scaled to the largest object plus one load of it; sessions beyond that roster still verifying at the cutoff move the close later) + one load of it {} s + one entrant lane of it {} s ({entrant_docs} documents, scaled from the measured lane); at most half of {} tempo(s) of {} s, {tempo_blocks} blocks at an assumed {block_secs} s a block); largest submission at most half the window ({} s of {} s)",
            d.admission_tempos,
            parsed.plan.transition_tempos,
            d.work_tempos,
            d.work_bound_secs,
            d.work_bound_secs - d.largest_upload_secs - d.verdict_queue_secs - d.load_secs - d.entrant_lane_secs,
            d.largest_upload_secs,
            bounds.object_bytes,
            d.verdict_queue_secs,
            d.verdict_rounds,
            slop_ninja_coordinator::arena_routes::STEP_READS_IN_FLIGHT,
            d.verdict_secs,
            d.load_secs,
            d.entrant_lane_secs,
            d.work_tempos,
            d.tempo_secs,
            d.largest_upload_secs,
            d.admission_tempos * d.tempo_secs,
        );
    }
    println!("{}", String::from_utf8(bytes).expect("utf-8"));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        fail(
            "a subcommand: load-plan, gate, summary, watch-line, tail-stats, snapshot-args, keys-check, copy-ties, chunk-keys, report-summary, restart, restart-cohort, authority-check, standing-template, probe-cut, template-reinstall",
        )
    };
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
        "restart" => restart(&flags),
        "restart-cohort" => restart_cohort(&flags),
        "authority-check" => authority_check(&flags),
        "standing-template" => standing_template(&flags),
        "probe-cut" => probe_cut(&flags),
        "template-reinstall" => template_reinstall(&flags),
        "template-check" => template_check(&flags),
        other => fail(&format!("unknown subcommand {other}")),
    }
}

#[cfg(test)]
mod p1f_tests {
    use super::*;

    /// P1-F: a switch build whose schema differs from the deployed store's is
    /// refused; an equal one passes; an unknown version is refused.
    #[test]
    fn a_switch_build_must_not_raise_the_deployed_schema() {
        assert!(switch_schema_check(&serde_json::json!({"schema_version": 28}), 28).is_ok());
        let e = switch_schema_check(&serde_json::json!({"schema_version": 27}), 28).unwrap_err();
        assert!(e.starts_with("switch_needs_migration"), "{e}");
        assert!(switch_schema_check(&serde_json::json!({}), 28).is_err());
    }
}

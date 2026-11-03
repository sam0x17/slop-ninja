//! Read-only view of `a_comp_pangram_bulk` collector stores (schema
//! `slop_ninja_a_comp_pangram_observation_v1`) under the competitive A
//! protocol v1.2 Pangram rules (`failures_and_missing.pangram`).
//!
//! Nothing here sends a request, reads prose or writes into a store. Each
//! selected document resolves to one outcome:
//!
//! - **valid**: a stored success whose echoed text matched exactly or up to
//!   whitespace, with in-range fractions and the expected text hash; the
//!   version pin is applied by the caller ([`Resolved::apply_pin`]);
//! - **missing**, with a reason: provider failure (including an echo mismatch
//!   beyond whitespace), a job not sent for budget, a quota-stop
//!   interruption, the provider's minimum length, a second uncertainty, an
//!   uncertain item never re-sent, a job never submitted, or another version.
//!
//! Stores are given in dispatch order. A store after the first may only
//! re-send items that were uncertain or never submitted in every earlier
//! store (protocol: "an item with an uncertain outcome may be re-sent once in
//! a new job after reconciliation"). A job that is still in flight, or whose
//! submission is uncertain, stops the read unless the operator declares it
//! uncertain (`store_index:job`), for example after its results expired.
//!
//! **Version pin rule** ([`first_success_version`]): the first store, in the
//! order given, holding a stored success; within it the first job in
//! `PLAN.json` order (the collector's sequential dispatch order) with a
//! stored success; within that job the success with the lowest `bulk_index`.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const OBSERVATION_SCHEMA: &str = "slop_ninja_a_comp_pangram_observation_v1";
pub const PLAN_SCHEMA: &str = "slop_ninja_a_comp_pangram_bulk_plan_v1";
pub const PIN_RULE: &str = "first store in dispatch order holding a stored success; within it the first job in PLAN.json order with a stored success; within that job the success with the lowest bulk_index";

/// A stored success (before the version pin).
#[derive(Clone, Debug, PartialEq)]
pub struct Success {
    pub version: String,
    pub fraction_ai: f64,
    pub fraction_ai_assisted: f64,
    pub echo: String,
    pub store: usize,
    pub job: String,
    pub bulk_index: u64,
}

impl Success {
    /// Comparator input `min(1, fraction_ai + fraction_ai_assisted)`.
    pub fn clamped_fraction(&self) -> f64 {
        (self.fraction_ai + self.fraction_ai_assisted).min(1.0)
    }
}

/// Final outcome of one selected document.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Valid(Success),
    Missing(String),
}

impl Outcome {
    pub fn valid(&self) -> Option<&Success> {
        match self {
            Outcome::Valid(s) => Some(s),
            Outcome::Missing(_) => None,
        }
    }
}

/// State of one item inside one store.
#[derive(Clone, Debug, PartialEq)]
enum State {
    Success(Success),
    /// Definitive: a failure response or a not-sent decision.
    Definitive(String),
    /// Uncertain outcome (declared by the operator for an in-flight,
    /// expired or unreceipted job).
    Uncertain(String),
    /// Job never submitted and no quota stop latched.
    NeverSent,
    /// Collector skipped a duplicate text; resolved to its twin.
    DuplicateText,
}

struct StoreView {
    root: PathBuf,
    split: String,
    plan_sha256: String,
    /// Item states keyed by id.
    items: BTreeMap<String, State>,
    /// Expected text hash per item.
    hashes: BTreeMap<String, String>,
    /// Successes in pin order: (job position, bulk_index, id).
    pin_order: Vec<(usize, u64, String)>,
    jobs: Vec<Value>,
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?)
        .with_context(|| p.display().to_string())
}

fn sha_file(p: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        fs::read(p).with_context(|| p.display().to_string())?,
    )))
}

fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|x| !x.is_empty())
        .with_context(|| format!("missing string {k}"))
}

fn frac(v: &Value, k: &str) -> Option<f64> {
    v[k].as_f64().filter(|x| x.is_finite() && (0.0..=1.0).contains(x))
}

/// Classify one stored `result.json` of an item of `job`.
fn observation_state(
    o: &Value,
    id: &str,
    split: &str,
    job: &str,
    text_sha256: &str,
    store: usize,
) -> Result<State> {
    ensure!(
        o["schema"] == OBSERVATION_SCHEMA,
        "{id}: unexpected observation schema"
    );
    ensure!(
        o["id"] == id && o["split"] == split && o["job"] == job,
        "{id}: observation binding (id, split or job) differs"
    );
    ensure!(
        o["text_sha256"].as_str() == Some(text_sha256),
        "{id}: observation text hash differs from the planned text"
    );
    ensure!(
        o["is_training_label"] == false,
        "{id}: observation marked as a training label"
    );
    match o["status"].as_str() {
        Some("success") => {
            let echo = s(o, "echo_match")?.to_string();
            if !matches!(echo.as_str(), "exact" | "whitespace_only") {
                return Ok(State::Definitive("echo_mismatch".into()));
            }
            let (Some(fa), Some(faa), Some(fh)) = (
                frac(o, "fraction_ai"),
                frac(o, "fraction_ai_assisted"),
                frac(o, "fraction_human"),
            ) else {
                return Ok(State::Definitive("invalid_fraction".into()));
            };
            if ((fa + faa + fh) - 1.0).abs() >= 1e-5 {
                return Ok(State::Definitive("invalid_fraction".into()));
            }
            let version = s(o, "version")?.to_string();
            let bulk_index = o["bulk_index"].as_u64().context("bulk_index")?;
            Ok(State::Success(Success {
                version,
                fraction_ai: fa,
                fraction_ai_assisted: faa,
                echo,
                store,
                job: job.to_string(),
                bulk_index,
            }))
        }
        Some("failure") => {
            let f = o["failure"].as_str().unwrap_or("");
            Ok(State::Definitive(if f.contains("echo mismatch") {
                "echo_mismatch".into()
            } else {
                "failure_response".into()
            }))
        }
        other => bail!("{id}: unknown observation status {other:?}"),
    }
}

fn open_store(root: &Path, index: usize, uncertain_jobs: &BTreeSet<String>) -> Result<StoreView> {
    let plan_p = root.join("PLAN.json");
    let plan = read_json(&plan_p)?;
    ensure!(plan["schema"] == PLAN_SCHEMA, "{}: not a bulk plan", plan_p.display());
    let split = s(&plan, "split")?.to_string();
    let quota_stop = root.join("quota-stop.json").exists();
    let mut items = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    let mut pin_order = vec![];
    for ns in plan["not_sent"].as_array().context("not_sent")? {
        let id = s(ns, "id")?.to_string();
        let reason = s(ns, "reason")?;
        let st = match reason {
            "duplicate_text_in_subsample" => State::DuplicateText,
            "below_provider_minimum_words" => State::Definitive("below_provider_minimum_words".into()),
            other => State::Definitive(format!("not_sent:{other}")),
        };
        ensure!(items.insert(id.clone(), st).is_none(), "{id}: listed twice");
    }
    let jobs = plan["jobs"].as_array().context("jobs")?.clone();
    let mut seen_jobs = BTreeSet::new();
    for (pos, j) in jobs.iter().enumerate() {
        let job = s(j, "job")?.to_string();
        ensure!(seen_jobs.insert(job.clone()), "job {job} listed twice");
        let dir = root.join("jobs").join(&job);
        let jp = read_json(&dir.join("plan.json"))?;
        ensure!(jp["split"] == split.as_str() && jp["job"] == job.as_str(), "{job}: plan binding");
        let complete = dir.join("complete.json").exists();
        let not_sent = dir.join("not-sent.json").exists();
        let receipt = dir.join("receipt.json").exists();
        let intent = dir.join("submission-intent.json").exists();
        let declared = uncertain_jobs.contains(&job);
        ensure!(
            !(declared && (complete || not_sent)),
            "{job} declared uncertain but is complete or not sent"
        );
        let mut successes = vec![];
        for it in jp["items"].as_array().context("plan items")? {
            let id = s(it, "id")?.to_string();
            let sha = s(it, "text_sha256")?.to_string();
            let st = if complete {
                let o = read_json(&root.join("obs").join(&id).join("result.json"))
                    .with_context(|| format!("{job} complete but {id} has no stored result"))?;
                let st = observation_state(&o, &id, &split, &job, &sha, index)?;
                if let State::Success(x) = &st {
                    successes.push((x.bulk_index, id.clone()));
                }
                st
            } else if not_sent {
                State::Definitive("budget_not_sent".into())
            } else if receipt || intent {
                ensure!(
                    declared,
                    "store {index} {job}: {} without stored results; collect it, or declare it uncertain ({index}:{job}) after reconciliation",
                    if receipt { "accepted job" } else { "submission intent" }
                );
                State::Uncertain(if receipt {
                    "accepted_results_not_stored".into()
                } else {
                    "uncertain_submission".into()
                })
            } else if quota_stop {
                State::Definitive("quota_stop_not_sent".into())
            } else {
                ensure!(!declared, "{job} declared uncertain but never submitted");
                State::NeverSent
            };
            ensure!(items.insert(id.clone(), st).is_none(), "{id}: planned twice");
            hashes.insert(id, sha);
        }
        successes.sort();
        pin_order.extend(successes.into_iter().map(|(b, id)| (pos, b, id)));
    }
    for j in uncertain_jobs {
        ensure!(seen_jobs.contains(j), "declared uncertain job {j} not in store {index}");
    }
    Ok(StoreView {
        root: root.to_path_buf(),
        split,
        plan_sha256: sha_file(&plan_p)?,
        items,
        hashes,
        pin_order,
        jobs,
    })
}

/// Resolved outcomes of a selection over one or more stores.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// Outcome before the version pin.
    pub outcomes: BTreeMap<String, Outcome>,
    /// The first success under [`PIN_RULE`]: (version, store, job, bulk_index, id).
    pub first_success: Option<(String, usize, String, u64, String)>,
    pub stores: Vec<Value>,
    pub duplicate_text_resolved: usize,
    pub uncertainties: usize,
}

/// Version string of the first success under [`PIN_RULE`].
pub fn first_success_version(r: &Resolved) -> Option<&str> {
    r.first_success.as_ref().map(|x| x.0.as_str())
}

impl Resolved {
    /// Apply the version pin: a success with another version is missing.
    pub fn apply_pin(&self, version: &str) -> BTreeMap<String, Outcome> {
        self.outcomes
            .iter()
            .map(|(id, o)| {
                let o = match o {
                    Outcome::Valid(x) if x.version != version => {
                        Outcome::Missing("version_mismatch".into())
                    }
                    o => o.clone(),
                };
                (id.clone(), o)
            })
            .collect()
    }
}

/// Missing-reason counts of pinned outcomes.
pub fn missing_counts(o: &BTreeMap<String, Outcome>) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for x in o.values() {
        if let Outcome::Missing(r) = x {
            *m.entry(r.clone()).or_default() += 1;
        }
    }
    m
}

/// Resolve every selected document (`id -> text_sha256`) over `stores` (in
/// dispatch order) of `split`. `uncertain` lists `store_index:job` jobs the
/// operator declared uncertain.
pub fn resolve(
    selected: &BTreeMap<String, String>,
    stores: &[PathBuf],
    split: &str,
    uncertain: &[String],
) -> Result<Resolved> {
    ensure!(!stores.is_empty(), "at least one store");
    let mut declared: Vec<BTreeSet<String>> = vec![BTreeSet::new(); stores.len()];
    for u in uncertain {
        let (i, j) = u
            .split_once(':')
            .with_context(|| format!("uncertain job {u}: expected STORE_INDEX:JOB"))?;
        let i: usize = i.parse().context("store index")?;
        ensure!(i < stores.len(), "uncertain job {u}: no store {i}");
        declared[i].insert(j.to_string());
    }
    let views = stores
        .iter()
        .enumerate()
        .map(|(i, p)| open_store(p, i, &declared[i]))
        .collect::<Result<Vec<_>>>()?;
    for (i, v) in views.iter().enumerate() {
        ensure!(v.split == split, "store {i} is split {} not {split}", v.split);
        for (id, _) in &v.items {
            ensure!(selected.contains_key(id), "store {i} holds unselected document {id}");
        }
        for (id, h) in &v.hashes {
            ensure!(
                selected[id] == *h,
                "store {i}: planned text hash of {id} differs from the selection"
            );
        }
    }
    let first: BTreeSet<&String> = views[0].items.keys().collect();
    let want: BTreeSet<&String> = selected.keys().collect();
    ensure!(
        first == want,
        "the first store must plan exactly the selected documents ({} planned, {} selected)",
        first.len(),
        want.len()
    );
    let mut outcomes = BTreeMap::new();
    let mut uncertainties = 0;
    let mut dups = vec![];
    for id in selected.keys() {
        let mut uncertain_seen = 0;
        let mut out: Option<Outcome> = None;
        let mut definitive_at: Option<usize> = None;
        for (i, v) in views.iter().enumerate() {
            let Some(st) = v.items.get(id) else { continue };
            ensure!(
                definitive_at.is_none(),
                "{id}: re-sent in store {i} after a definitive outcome in store {}",
                definitive_at.unwrap()
            );
            match st {
                State::Success(x) => {
                    out = Some(Outcome::Valid(x.clone()));
                    definitive_at = Some(i);
                }
                State::Definitive(r) => {
                    out = Some(Outcome::Missing(r.clone()));
                    definitive_at = Some(i);
                }
                State::DuplicateText => {
                    ensure!(i == 0, "{id}: duplicate-text skip outside the first store");
                    out = None;
                    definitive_at = Some(i);
                    dups.push(id.clone());
                }
                State::Uncertain(r) => {
                    uncertain_seen += 1;
                    uncertainties += 1;
                    out = Some(Outcome::Missing(if uncertain_seen >= 2 {
                        "second_uncertainty".into()
                    } else {
                        format!("uncertain_not_resent:{r}")
                    }));
                    if uncertain_seen >= 2 {
                        definitive_at = Some(i);
                    }
                }
                State::NeverSent => {
                    out = Some(Outcome::Missing("never_sent".into()));
                }
            }
        }
        if let Some(o) = out {
            outcomes.insert(id.clone(), o);
        }
    }
    // One observation per exact text: a duplicate-text skip takes its sent
    // twin's outcome.
    let mut by_hash: BTreeMap<&str, &str> = BTreeMap::new();
    for (id, h) in selected {
        if outcomes.contains_key(id) {
            by_hash.entry(h.as_str()).or_insert(id.as_str());
        }
    }
    let dup_n = dups.len();
    for id in dups {
        let twin = by_hash
            .get(selected[&id].as_str())
            .with_context(|| format!("{id}: duplicate-text skip without a sent twin"))?;
        let o = outcomes[*twin].clone();
        outcomes.insert(id, o);
    }
    let mut first_success = None;
    'outer: for (i, v) in views.iter().enumerate() {
        for (_, b, id) in &v.pin_order {
            if let Some(State::Success(x)) = v.items.get(id) {
                first_success = Some((x.version.clone(), i, x.job.clone(), *b, id.clone()));
                break 'outer;
            }
        }
    }
    let stores_json = views
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({"index": i, "root": v.root, "plan_sha256": v.plan_sha256,
                   "items": v.items.len(), "jobs": v.jobs.len(),
                   "declared_uncertain_jobs": declared[i]})
        })
        .collect();
    Ok(Resolved {
        outcomes,
        first_success,
        stores: stores_json,
        duplicate_text_resolved: dup_n,
        uncertainties,
    })
}

/// Digest of the pinned outcomes (id, status, version, fractions as bits),
/// binding the exact observations an analysis used.
pub fn outcomes_digest(o: &BTreeMap<String, Outcome>) -> String {
    let mut h = Sha256::new();
    h.update(b"slop_ninja_a_comp_pangram_outcomes_v1\0");
    for (id, x) in o {
        let line = match x {
            Outcome::Valid(s) => format!(
                "{id}\tvalid\t{}\t{:016x}\t{:016x}\n",
                s.version,
                s.fraction_ai.to_bits(),
                s.fraction_ai_assisted.to_bits()
            ),
            Outcome::Missing(r) => format!("{id}\tmissing\t{r}\n"),
        };
        h.update(line.as_bytes());
    }
    hex::encode(h.finalize())
}

#[cfg(test)]
pub(crate) mod testkit {
    //! Synthetic collector stores for tests (no network, no prose).
    use super::*;

    pub struct Item {
        pub id: String,
        pub sha: String,
        /// None: no stored result (job state decides).
        pub result: Option<Value>,
    }

    pub fn success(id: &str, sha: &str, job: &str, idx: u64, version: &str, fa: f64, faa: f64) -> Value {
        json!({"schema": OBSERVATION_SCHEMA, "id": id, "split": "cal", "model": "pangram-4",
               "text_sha256": sha, "bulk_index": idx, "job": job, "is_training_label": false,
               "status": "success", "version": version, "echo_match": "exact",
               "fraction_ai": fa, "fraction_ai_assisted": faa, "fraction_human": 1.0 - fa - faa})
    }

    pub fn failure(id: &str, sha: &str, job: &str, idx: u64, why: &str) -> Value {
        json!({"schema": OBSERVATION_SCHEMA, "id": id, "split": "cal", "text_sha256": sha,
               "bulk_index": idx, "job": job, "is_training_label": false, "status": "failure",
               "failure": why})
    }

    /// Write a store: jobs as (name, state, items) with state one of
    /// "complete", "not_sent", "receipt", "intent", "none".
    pub fn write_store(root: &Path, split: &str, not_sent: &[(&str, &str)], jobs: &[(&str, &str, Vec<Item>)]) {
        fs::create_dir_all(root.join("jobs")).unwrap();
        fs::create_dir_all(root.join("obs")).unwrap();
        let mut list = vec![];
        for (name, state, items) in jobs {
            let dir = root.join("jobs").join(name);
            fs::create_dir_all(&dir).unwrap();
            let plan = json!({"split": split, "job": name,
                "items": items.iter().map(|i| json!({"id": i.id, "text_sha256": i.sha, "billed_units": 1})).collect::<Vec<_>>()});
            fs::write(dir.join("plan.json"), plan.to_string()).unwrap();
            match *state {
                "complete" => fs::write(dir.join("complete.json"), "{}").unwrap(),
                "not_sent" => fs::write(dir.join("not-sent.json"), "{}").unwrap(),
                "receipt" => fs::write(dir.join("receipt.json"), "{}").unwrap(),
                "intent" => fs::write(dir.join("submission-intent.json"), "{}").unwrap(),
                _ => {}
            }
            for i in items {
                if let Some(r) = &i.result {
                    let mut r = r.clone();
                    r["split"] = json!(split);
                    let d = root.join("obs").join(&i.id);
                    fs::create_dir_all(&d).unwrap();
                    fs::write(d.join("result.json"), r.to_string()).unwrap();
                }
            }
            list.push(json!({"job": name}));
        }
        let plan = json!({"schema": PLAN_SCHEMA, "split": split,
            "not_sent": not_sent.iter().map(|(id, r)| json!({"id": id, "reason": r})).collect::<Vec<_>>(),
            "jobs": list});
        fs::write(root.join("PLAN.json"), plan.to_string()).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    fn it(id: &str, r: Option<Value>) -> Item {
        Item { id: id.into(), sha: format!("h{id}"), result: r }
    }

    #[test]
    fn resolves_outcomes_pin_and_missing_reasons() {
        let d = tempfile::tempdir().unwrap();
        let st = d.path().join("s0");
        write_store(
            &st,
            "cal",
            &[("short", "below_provider_minimum_words"), ("dup", "duplicate_text_in_subsample")],
            &[
                ("job-000", "not_sent", vec![it("b1", None)]),
                (
                    "job-001",
                    "complete",
                    vec![
                        it("a2", Some(success("a2", "ha2", "job-001", 2, "4.1", 0.5, 0.25))),
                        it("a0", Some(success("a0", "ha0", "job-001", 0, "4.1", 0.1, 0.0))),
                        it("a1", Some(success("a1", "ha1", "job-001", 1, "4.2", 0.2, 0.0))),
                        it("f", Some(failure("f", "hf", "job-001", 3, "provider item failure"))),
                        it("e", Some(failure("e", "he", "job-001", 4, "echo mismatch beyond whitespace"))),
                    ],
                ),
                ("job-002", "none", vec![it("n", None)]),
            ],
        );
        let mut sel: BTreeMap<String, String> = ["b1", "a0", "a1", "a2", "f", "e", "n", "short"]
            .iter()
            .map(|x| (x.to_string(), format!("h{x}")))
            .collect();
        sel.insert("dup".into(), "ha2".into());
        let r = resolve(&sel, &[st.clone()], "cal", &[]).unwrap();
        assert_eq!(first_success_version(&r), Some("4.1"));
        assert_eq!(r.first_success.as_ref().unwrap().4, "a0");
        let p = r.apply_pin("4.1");
        assert_eq!(p["a0"].valid().unwrap().clamped_fraction(), 0.1);
        assert_eq!(p["a1"], Outcome::Missing("version_mismatch".into()));
        assert_eq!(p["b1"], Outcome::Missing("budget_not_sent".into()));
        assert_eq!(p["f"], Outcome::Missing("failure_response".into()));
        assert_eq!(p["e"], Outcome::Missing("echo_mismatch".into()));
        assert_eq!(p["n"], Outcome::Missing("never_sent".into()));
        assert_eq!(p["short"], Outcome::Missing("below_provider_minimum_words".into()));
        // The duplicate takes its twin's observation (one observation per text).
        assert_eq!(p["dup"], p["a2"]);
        assert_eq!(r.duplicate_text_resolved, 1);
        let c = missing_counts(&p);
        assert_eq!(c.values().sum::<usize>(), 6);
        // Wrong split, unselected documents and hash mismatches are refused.
        assert!(resolve(&sel, &[st.clone()], "test", &[]).is_err());
        let mut fewer = sel.clone();
        fewer.remove("n");
        assert!(resolve(&fewer, &[st.clone()], "cal", &[]).is_err());
        let mut bad = sel.clone();
        bad.insert("a0".into(), "other".into());
        assert!(resolve(&bad, &[st.clone()], "cal", &[]).is_err());
        assert_ne!(outcomes_digest(&p), outcomes_digest(&r.apply_pin("4.2")));
    }

    #[test]
    fn in_flight_jobs_need_declaration_and_resend_rules_hold() {
        let d = tempfile::tempdir().unwrap();
        let s0 = d.path().join("s0");
        write_store(
            &s0,
            "test",
            &[],
            &[
                ("job-000", "complete", vec![it("a", Some(success("a", "ha", "job-000", 0, "4.1", 0.3, 0.0)))]),
                ("job-001", "receipt", vec![it("u", None), it("w", None)]),
            ],
        );
        let sel: BTreeMap<String, String> =
            ["a", "u", "w"].iter().map(|x| (x.to_string(), format!("h{x}"))).collect();
        assert!(resolve(&sel, &[s0.clone()], "test", &[]).is_err(), "in-flight job");
        let r = resolve(&sel, &[s0.clone()], "test", &["0:job-001".into()]).unwrap();
        assert!(matches!(&r.outcomes["u"], Outcome::Missing(x) if x.starts_with("uncertain_not_resent")));
        // Re-send of the uncertain items: one succeeds, one is uncertain again.
        let s1 = d.path().join("s1");
        write_store(
            &s1,
            "test",
            &[],
            &[
                ("job-000", "complete", vec![it("u", Some(success("u", "hu", "job-000", 0, "4.1", 0.0, 0.9)))]),
                ("job-001", "intent", vec![it("w", None)]),
            ],
        );
        let r = resolve(&sel, &[s0.clone(), s1.clone()], "test", &["0:job-001".into(), "1:job-001".into()]).unwrap();
        assert_eq!(r.outcomes["u"].valid().unwrap().store, 1);
        assert_eq!(r.outcomes["w"], Outcome::Missing("second_uncertainty".into()));
        assert_eq!(r.first_success.as_ref().unwrap().4, "a");
        // A definitive item re-sent in a later store is refused.
        let s2 = d.path().join("s2");
        write_store(&s2, "test", &[], &[("job-000", "complete", vec![it("a", Some(success("a", "ha", "job-000", 0, "4.1", 0.3, 0.0)))])]);
        assert!(resolve(&sel, &[s0, s2], "test", &["0:job-001".into()]).is_err());
    }
}

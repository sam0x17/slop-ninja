//! Exercise the production CLI without granting the subprocess a detector key.
//! Valid cached runs require no network; invalid runs must fail preflight.

use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;
use unslop::{
    provider_ids::{MAP_ENV, SALT_ENV, is_digest},
    util::digest,
};

const SALT: &[u8] = b"synthetic-provider-id-salt-v1";

struct Fixture {
    _root: TempDir,
    baseline: PathBuf,
    candidate: PathBuf,
    out: PathBuf,
    a: String,
    b: String,
}

impl Fixture {
    /// The CLI with a synthetic salt and a temporary sidecar map. Env changes
    /// affect only the child process.
    fn unslop(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_unslop"));
        command
            .env_remove("PANGRAM_API_KEY")
            .env(SALT_ENV, self._root.path().join("provider-id-salt"))
            .env(MAP_ENV, self.map());
        command
    }

    fn map(&self) -> PathBuf {
        self._root.path().join("private").join("id-map.tsv")
    }

    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let baseline = root.path().join("baseline.txt");
        let candidate = root.path().join("candidate.txt");
        let out = root.path().join("study");
        let a = "A reader can follow this philosophical argument in some detail. ".repeat(6);
        let b = "A reader can examine this philosophical argument in some detail. ".repeat(6);
        fs::write(&baseline, &a).unwrap();
        fs::write(&candidate, &b).unwrap();
        fs::write(root.path().join("provider-id-salt"), SALT).unwrap();
        Self {
            _root: root,
            baseline,
            candidate,
            out,
            a,
            b,
        }
    }

    fn command(&self, max_requests: usize) -> Command {
        let mut command = self.unslop();
        command
            .arg("study")
            .arg(&self.baseline)
            .arg(&self.candidate)
            .arg("--out")
            .arg(&self.out)
            .arg("--max-requests")
            .arg(max_requests.to_string());
        command
    }

    fn score_command(&self, out: &std::path::Path, budget: usize) -> Command {
        let mut command = self.unslop();
        command
            .arg("score")
            .arg(&self.baseline)
            .arg("--out")
            .arg(out)
            .arg("--max-requests")
            .arg(budget.to_string());
        command
    }

    fn corpus(&self, records: &[Value]) -> PathBuf {
        let path = self._root.path().join("corpus.jsonl");
        let text: String = records.iter().map(|d| format!("{d}\n")).collect();
        fs::write(&path, text).unwrap();
        path
    }

    fn corpus_command(&self, corpus: &std::path::Path, budget: usize) -> Command {
        let mut command = self.unslop();
        command
            .arg("score-corpus")
            .arg(corpus)
            .arg("--out")
            .arg(&self.out)
            .arg("--max-requests")
            .arg(budget.to_string());
        command
    }

    fn cache_corpus(&self, record: &Value) -> PathBuf {
        fs::create_dir_all(&self.out).unwrap();
        let path = self
            .out
            .join(format!("{}.json", record["sha256"].as_str().unwrap()));
        fs::write(&path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
        path
    }

    fn record(&self, role: &str, index: usize) -> Value {
        let text = if role == "baseline" { &self.a } else { &self.b };
        let ai = if role == "baseline" { 0.7 } else { 0.04 };
        json!({"detector": "pangram", "model": "pangram-4", "task_id": format!("{role}-{index}"),
            "submitted_text": text, "sha256": digest(text), "full_document": true,
            "role": role, "block": index,
            "result": {"stage": "STAGE_SUCCESS", "version": "4.0", "fraction_ai": ai,
                "fraction_ai_assisted": 0.01, "fraction_human": 1.0-ai-0.01}})
    }

    fn cache(&self, role: &str, index: usize, record: &Value) -> PathBuf {
        fs::create_dir_all(&self.out).unwrap();
        let path = self.out.join(format!("{:02}-{role}.json", index + 1));
        fs::write(&path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
        path
    }

    fn complete_cache(&self) {
        for i in 0..3 {
            for role in ["baseline", "candidate"] {
                self.cache(role, i, &self.record(role, i));
            }
        }
    }

    fn audit(&self) -> PathBuf {
        let path = self._root.path().join("audit.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "source_sha256": digest(&self.a), "candidate_sha256": digest(&self.b),
                "reviewed_by_human": true, "readability": true, "argumentation": true,
                "detail": true, "tone": true,
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }
}

fn attributed_model_document(text: &str, group: &str, split: &str) -> Value {
    json!({"text":text,"corpus":"integration-attributed-model","source_kind":"model",
        "provider":"fixture-provider","model":"fixture-model-v1","domain":"philosophy",
        "register":"book-preface","group_id":group,"split":split,
        "metadata":{"fixture":true,"model_identity_source":"test fixture"}})
}

fn failure(output: Output, expected: &str) {
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success(), "Unexpected success");
    assert!(
        stderr.to_lowercase().contains(&expected.to_lowercase()),
        "Expected {expected:?}; got {stderr}"
    );
    assert!(
        !stderr.contains("Set PANGRAM_API_KEY"),
        "Validation happened after client construction: {stderr}"
    );
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn complete_study_resumes_from_cache_without_key_and_quality_audit_is_required() {
    let f = Fixture::new();
    f.complete_cache();
    let report = success(f.command(0).arg("--full-document").output().unwrap());
    assert_eq!(report["observed_success"], false);
    assert_eq!(report["candidate"]["all_groups_observed_success"], true);
    assert_eq!(report["reliability_established"], false);
    assert!(f.out.join("summary.json").exists());
    let report = success(
        f.command(0)
            .arg("--full-document")
            .arg("--audit")
            .arg(f.audit())
            .output()
            .unwrap(),
    );
    assert_eq!(report["observed_success"], true);
    assert_eq!(report["baseline"]["unique_runs"], 3);
    assert_eq!(report["candidate"]["unique_runs"], 3);
}

#[test]
fn completed_study_imports_each_task_once_and_replays_from_sqlite() {
    let f = Fixture::new();
    f.complete_cache();
    success(f.command(0).arg("--full-document").output().unwrap());
    let db = f._root.path().join("study.sqlite3");
    for expected in [6, 0] {
        let output = f
            .unslop()
            .arg("--db")
            .arg(&db)
            .arg("import-pangram")
            .arg(&f.out)
            .args([
                "--corpus",
                "cached-study",
                "--group-id",
                "test-source-family",
            ])
            .output()
            .unwrap();
        let imported = success(output);
        assert_eq!(imported["imported_runs"], expected);
        assert_eq!(imported["non_run_files_skipped"], 2);
    }
    let output = f
        .unslop()
        .arg("--db")
        .arg(&db)
        .args(["runs", "--corpus", "cached-study"])
        .output()
        .unwrap();
    let summary = success(output);
    assert_eq!(summary["unique_runs"], 6);
    assert_eq!(summary["groups"].as_array().unwrap().len(), 2);
    for group in summary["groups"].as_array().unwrap() {
        let ids = group["task_ids"].as_array().unwrap();
        assert!(ids.iter().all(|id| is_digest(id.as_str().unwrap())));
    }
}

#[test]
fn study_summary_lists_task_digests_and_raw_ids_stay_in_the_private_sidecar() {
    let f = Fixture::new();
    f.complete_cache();
    let report = success(f.command(0).arg("--full-document").output().unwrap());
    let written = fs::read_to_string(f.out.join("summary.json")).unwrap();
    for arm in ["baseline", "candidate"] {
        let ids = report[arm]["groups"][0]["task_ids"].as_array().unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| is_digest(id.as_str().unwrap())));
        for i in 0..3 {
            assert!(
                !written.contains(&format!("\"{arm}-{i}\"")),
                "raw id in summary"
            );
        }
    }
    let map = fs::read_to_string(f.map()).unwrap();
    assert_eq!(map.lines().next(), Some("provider_id\tdigest\tfile"));
    assert_eq!(map.lines().count(), 7);
    let summary_path = f.out.join("summary.json").display().to_string();
    assert!(map.lines().any(|line| {
        let fields: Vec<_> = line.split('\t').collect();
        fields.len() == 3
            && fields[0] == "baseline-0"
            && is_digest(fields[1])
            && fields[2] == summary_path
    }));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &std::path::Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&f.map()), 0o600);
        assert_eq!(mode(f.map().parent().unwrap()), 0o700);
    }
    // A rerun adds no duplicate rows.
    success(f.command(0).arg("--full-document").output().unwrap());
    assert_eq!(fs::read_to_string(f.map()).unwrap().lines().count(), 7);
}

#[test]
fn missing_salt_fails_before_any_request_or_output() {
    let f = Fixture::new();
    let absent = f._root.path().join("absent-salt");
    failure(
        f.command(6)
            .arg("--full-document")
            .env(SALT_ENV, &absent)
            .output()
            .unwrap(),
        "provider-id salt",
    );
    assert!(!f.out.exists());
    let out = f._root.path().join("score.json");
    failure(
        f.score_command(&out, 1)
            .env(SALT_ENV, &absent)
            .output()
            .unwrap(),
        "provider-id salt",
    );
    assert!(!out.exists() && !f.map().exists());
}

#[test]
fn invalid_settings_fail_before_client_construction() {
    for (flag, value) in [
        ("--threshold", "NaN"),
        ("--threshold", "0"),
        ("--threshold", "1.1"),
        ("--timeout", "0"),
        ("--repeats", "0"),
        ("--model", " "),
    ] {
        let f = Fixture::new();
        failure(
            f.command(6)
                .arg("--full-document")
                .args([flag, value])
                .output()
                .unwrap(),
            "Invalid study settings",
        );
        assert!(!f.out.exists());
    }
}

#[test]
fn both_inputs_are_validated_before_requests() {
    for role in ["baseline", "candidate"] {
        let f = Fixture::new();
        fs::write(
            if role == "baseline" {
                &f.baseline
            } else {
                &f.candidate
            },
            "Too short.",
        )
        .unwrap();
        failure(
            f.command(6).arg("--full-document").output().unwrap(),
            "50 words",
        );
        assert!(!f.out.exists());
    }
    let f = Fixture::new();
    fs::write(&f.candidate, &f.a).unwrap();
    failure(
        f.command(6).arg("--full-document").output().unwrap(),
        "Candidate equals baseline",
    );
}

#[test]
fn malformed_and_wrong_hash_audits_fail_before_requests() {
    for audit in [json!(true), json!({"source_sha256": "wrong"})] {
        let f = Fixture::new();
        let path = f._root.path().join("bad-audit.json");
        fs::write(&path, serde_json::to_vec(&audit).unwrap()).unwrap();
        let expected = if audit.is_object() {
            "exact source/candidate hashes"
        } else {
            "must be an object"
        };
        failure(
            f.command(6)
                .arg("--full-document")
                .arg("--audit")
                .arg(path)
                .output()
                .unwrap(),
            expected,
        );
        assert!(!f.out.exists());
    }
}

#[test]
fn missing_and_conflicting_scope_are_parser_errors() {
    let f = Fixture::new();
    failure(f.command(6).output().unwrap(), "--full-document");
    failure(
        f.command(6)
            .args(["--full-document", "--section"])
            .output()
            .unwrap(),
        "cannot be used",
    );
    assert!(!f.out.exists());
}

#[test]
fn request_budget_is_checked_for_whole_study() {
    let f = Fixture::new();
    failure(
        f.command(1).arg("--full-document").output().unwrap(),
        "needs 6 new paid requests",
    );
    assert!(!f.out.join("01-baseline.json").exists());
}

#[test]
fn uncertain_late_cached_submission_blocks_all_new_submissions() {
    let f = Fixture::new();
    let mut record = f.record("candidate", 2);
    record.as_object_mut().unwrap().remove("task_id");
    record.as_object_mut().unwrap().remove("result");
    record["state"] = json!("submission_uncertain");
    let path = f.cache("candidate", 2, &record);
    let exact_record = fs::read(&path).unwrap();
    for _ in 0..2 {
        failure(
            f.command(6).arg("--full-document").output().unwrap(),
            "Uncertain prior submission",
        );
        assert_eq!(fs::read(&path).unwrap(), exact_record);
        assert!(!f.out.join("01-baseline.json").exists());
    }
}

#[test]
fn invalid_late_cached_success_is_checked_before_missing_early_slots() {
    let f = Fixture::new();
    let mut record = f.record("candidate", 2);
    record["result"]["fraction_ai"] = json!(2.0);
    f.cache("candidate", 2, &record);
    failure(
        f.command(6).arg("--full-document").output().unwrap(),
        "fraction_ai",
    );
    assert!(!f.out.join("01-baseline.json").exists());
}

#[test]
fn cache_checks_exact_bytes_and_scope_before_requests() {
    for field in ["submitted_text", "sha256", "model", "full_document"] {
        let f = Fixture::new();
        let mut record = f.record("candidate", 2);
        record[field] = match field {
            "submitted_text" => json!(format!("{}\n", f.b)),
            "full_document" => json!(false),
            _ => json!("wrong"),
        };
        f.cache("candidate", 2, &record);
        failure(
            f.command(6).arg("--full-document").output().unwrap(),
            "Cached study record mismatch",
        );
        assert!(!f.out.join("01-baseline.json").exists());
    }
}

#[test]
fn cached_failed_task_prevents_new_requests() {
    let f = Fixture::new();
    let mut record = f.record("candidate", 2);
    record["result"]["stage"] = json!("STAGE_FAILED");
    f.cache("candidate", 2, &record);
    failure(
        f.command(6).arg("--full-document").output().unwrap(),
        "Previously failed detector task",
    );
}

#[test]
fn cached_duplicate_completed_task_does_not_count_as_distinct_repeat() {
    let f = Fixture::new();
    let record = f.record("baseline", 1);
    f.cache("baseline", 1, &record);
    f.cache("baseline", 2, &record);
    failure(
        f.command(6).arg("--full-document").output().unwrap(),
        "reuse a task ID",
    );
    assert!(!f.out.join("01-baseline.json").exists());
}

#[test]
fn cached_pending_task_ids_must_be_distinct_before_new_requests() {
    for other_completed in [false, true] {
        let f = Fixture::new();
        let mut pending = f.record("baseline", 1);
        pending.as_object_mut().unwrap().remove("result");
        f.cache("baseline", 1, &pending);
        let other = if other_completed {
            f.record("baseline", 1)
        } else {
            pending
        };
        f.cache("baseline", 2, &other);
        failure(
            f.command(6).arg("--full-document").output().unwrap(),
            "reuse a task ID",
        );
        assert!(!f.out.join("01-baseline.json").exists());
    }
}

#[test]
fn cached_other_detector_is_not_polled_as_pangram() {
    let f = Fixture::new();
    let mut record = f.record("candidate", 2);
    record["detector"] = json!("other-detector");
    f.cache("candidate", 2, &record);
    failure(
        f.command(6).arg("--full-document").output().unwrap(),
        "Cached detector must be pangram",
    );
}

#[test]
fn unicode_and_crlf_input_hashes_remain_exact() {
    let mut f = Fixture::new();
    f.a = "Café readers can follow this philosophical argument in some detail.\r\n".repeat(6);
    f.b = "Café readers can examine this philosophical argument in some detail.\r\n".repeat(6);
    fs::write(&f.baseline, &f.a).unwrap();
    fs::write(&f.candidate, &f.b).unwrap();
    f.complete_cache();
    let report = success(f.command(0).arg("--full-document").output().unwrap());
    assert_eq!(report["baseline"]["groups"][0]["sha256"], digest(&f.a));
    assert_ne!(digest(&f.a), digest(&f.a.replace("\r\n", "\n")));
    let cached: Value =
        serde_json::from_slice(&fs::read(f.out.join("01-baseline.json")).unwrap()).unwrap();
    assert_eq!(cached["submitted_text"], f.a);
}

#[test]
fn score_resumes_complete_exact_record_without_key_or_budget() {
    let f = Fixture::new();
    let out = f._root.path().join("score.json");
    fs::write(&out, serde_json::to_vec(&f.record("baseline", 0)).unwrap()).unwrap();
    let report = success(f.score_command(&out, 0).output().unwrap());
    assert_eq!(report["unique_runs"], 1);
    assert_eq!(report["all_groups_observed_success"], false);
    assert_eq!(report["groups"][0]["sha256"], digest(&f.a));
}

#[test]
fn score_rejects_uncertain_submission_and_changed_input_before_requests() {
    let f = Fixture::new();
    let out = f._root.path().join("score.json");
    let mut record = f.record("baseline", 0);
    record.as_object_mut().unwrap().remove("task_id");
    fs::write(&out, serde_json::to_vec(&record).unwrap()).unwrap();
    failure(
        f.score_command(&out, 1).output().unwrap(),
        "Uncertain prior submission",
    );
    record["task_id"] = json!("baseline-0");
    record["submitted_text"] = json!(format!("{}\n", f.a));
    fs::write(&out, serde_json::to_vec(&record).unwrap()).unwrap();
    failure(
        f.score_command(&out, 1).output().unwrap(),
        "Score record input mismatch",
    );
}

#[test]
fn score_corpus_defaults_to_train_and_leaves_dev_test_unqueried() {
    let f = Fixture::new();
    let test_text = format!("{} Another separate source family.", f.b);
    let path = f.corpus(&[
        attributed_model_document(&f.a, "train-family", "train"),
        attributed_model_document(&f.b, "dev-family", "dev"),
        attributed_model_document(&test_text, "test-family", "test"),
    ]);
    f.cache_corpus(&f.record("baseline", 0));
    let report = success(f.corpus_command(&path, 0).output().unwrap());
    assert_eq!(report["documents"], 1);
    assert_eq!(report["new_requests"], 0);
    assert_eq!(report["split"], "train");
    assert_eq!(report["observations"][0]["source_kind"], "model");
    assert_eq!(report["observations"][0]["group_id"], "train-family");
    assert_eq!(report["reliability_established"], false);
    assert_eq!(
        fs::read_to_string(f.out.join(format!("{}.txt", digest(&f.a)))).unwrap(),
        f.a
    );
    for text in [&f.b, &test_text] {
        assert!(!f.out.join(format!("{}.json", digest(text))).exists());
        assert!(!f.out.join(format!("{}.txt", digest(text))).exists());
    }
}

#[test]
fn score_corpus_checks_whole_budget_and_late_cache_before_first_slot() {
    let f = Fixture::new();
    let path = f.corpus(&[
        attributed_model_document(&f.a, "first-family", "train"),
        attributed_model_document(&f.b, "late-family", "train"),
    ]);
    failure(
        f.corpus_command(&path, 1).output().unwrap(),
        "Needs 2 new requests",
    );
    assert!(!f.out.exists());
    let mut late = f.record("candidate", 0);
    late["result"]["fraction_ai"] = json!(2.0);
    f.cache_corpus(&late);
    failure(f.corpus_command(&path, 1).output().unwrap(), "fraction_ai");
    assert!(!f.out.join(format!("{}.txt", digest(&f.a))).exists());
    assert!(!f.out.join(format!("{}.json", digest(&f.a))).exists());
    late.as_object_mut().unwrap().remove("result");
    late.as_object_mut().unwrap().remove("task_id");
    f.cache_corpus(&late);
    failure(
        f.corpus_command(&path, 1).output().unwrap(),
        "Uncertain prior corpus submission",
    );
    assert!(!f.out.join(format!("{}.txt", digest(&f.a))).exists());

    let mut wrong_detector = f.record("candidate", 0);
    wrong_detector["detector"] = json!("other-detector");
    f.cache_corpus(&wrong_detector);
    failure(
        f.corpus_command(&path, 1).output().unwrap(),
        "Cached detector must be pangram",
    );
    assert!(!f.out.join(format!("{}.json", digest(&f.a))).exists());
    assert!(!f.out.join(format!("{}.txt", digest(&f.a))).exists());

    let unscored = format!("{} A third independent source family.", f.a);
    let path = f.corpus(&[
        attributed_model_document(&unscored, "unscored-first-family", "train"),
        attributed_model_document(&f.a, "cached-family-a", "train"),
        attributed_model_document(&f.b, "cached-family-b", "train"),
    ]);
    for pending_role in ["baseline", "candidate"] {
        let mut first = f.record("baseline", 0);
        let mut last = f.record("candidate", 0);
        last["task_id"] = first["task_id"].clone();
        if pending_role == "baseline" {
            first.as_object_mut().unwrap().remove("result");
        } else {
            last.as_object_mut().unwrap().remove("result");
        }
        f.cache_corpus(&first);
        f.cache_corpus(&last);
        failure(
            f.corpus_command(&path, 1).output().unwrap(),
            "Cached corpus slots reuse a task ID",
        );
        assert!(!f.out.join(format!("{}.json", digest(&unscored))).exists());
        assert!(!f.out.join(format!("{}.txt", digest(&unscored))).exists());
    }
}

#[test]
fn attach_scores_preserves_model_provenance_and_is_idempotent() {
    let f = Fixture::new();
    let path = f.corpus(&[
        attributed_model_document(&f.a, "train-family", "train"),
        attributed_model_document(&f.b, "dev-family", "dev"),
    ]);
    f.cache_corpus(&f.record("baseline", 0));
    success(f.corpus_command(&path, 0).output().unwrap());
    let db = f._root.path().join("attributed.sqlite3");
    let ingested = success(
        f.unslop()
            .arg("--db")
            .arg(&db)
            .arg("ingest")
            .arg(path)
            .output()
            .unwrap(),
    );
    assert_eq!(ingested["records"], 2);
    for expected in [1, 0] {
        let attached = success(
            f.unslop()
                .arg("--db")
                .arg(&db)
                .arg("attach-scores")
                .args(["--corpus", "integration-attributed-model"])
                .arg(&f.out)
                .output()
                .unwrap(),
        );
        assert_eq!(attached["attached_runs"], expected);
    }
    let connection = rusqlite::Connection::open(&db).unwrap();
    let row: (String,String,String,String,String,String) = connection.query_row(
        "SELECT d.corpus,d.source_kind,d.provider,d.model,d.group_id,d.split FROM documents d JOIN detector_runs r ON r.document_id=d.id",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).unwrap();
    assert_eq!(
        row,
        (
            "integration-attributed-model".into(),
            "model".into(),
            "fixture-provider".into(),
            "fixture-model-v1".into(),
            "train-family".into(),
            "train".into()
        )
    );
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM documents WHERE source_kind='experimental'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    let report = success(
        f.unslop()
            .arg("--db")
            .arg(db)
            .args(["runs", "--corpus", "integration-attributed-model"])
            .output()
            .unwrap(),
    );
    assert_eq!(report["unique_runs"], 1);
}

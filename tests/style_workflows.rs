//! Offline CLI workflows with declared synthetic references and collected-shaped proposals.

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use unslop::util::{digest, read_jsonl};

const AUTHOR: &str = "synthetic-test-writer";
const REGISTER: &str = "synthetic-test-register";
const REFERENCE_A: &str = "The clerk filed each report. The manager checked every page.";
const REFERENCE_B: &str = "The farmer stored fresh apples. The driver checked each crate.";
const SOURCE: &str = "The sensor measured 20 samples, and the team checked every reading.";
const CANDIDATE: &str = "The sensor measured 20 samples. The team checked every reading.";

fn sample(id: &str, group: &str, split: &str, text: &str) -> Value {
    json!({"id":id,"author_id":AUTHOR,"register":REGISTER,"group_id":group,
        "text":text,"split":split,"authorship":"synthetic_fixture"})
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_string_pretty(value).unwrap() + "\n").unwrap();
}

fn write_jsonl(path: &Path, rows: &[Value]) {
    let bytes: String = rows.iter().map(|row| format!("{row}\n")).collect();
    fs::write(path, bytes).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn failure(output: Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "Unexpected command success");
    assert!(
        stderr.contains(expected),
        "Expected {expected:?}, got {stderr}"
    );
}

struct Fixture {
    root: TempDir,
    samples: PathBuf,
    profile: PathBuf,
    controls: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(source_text: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let samples = root.path().join("samples.jsonl");
        let profile = root.path().join("profile.json");
        let controls = root.path().join("rhythm-controls.json");
        let source = root.path().join("source.txt");
        write_jsonl(
            &samples,
            &[
                sample("sample-a", "work-a", "train", REFERENCE_A),
                sample("sample-b", "work-b", "train", REFERENCE_B),
            ],
        );
        fs::write(&source, source_text).unwrap();
        let f = Self {
            root,
            samples,
            profile,
            controls,
            source,
        };
        success(f.fit_command(&f.samples, &f.profile).output().unwrap());
        let initial_controls = f.root.path().join("default-controls.json");
        let mut controls = success(
            f.controls_command(&f.profile, &initial_controls, "0")
                .output()
                .unwrap(),
        );
        let artifact = read_json(&f.profile);
        // An explicit, interpretable objective isolates rhythm for this workflow test.
        // References and proposals use different topics, so word overlap is irrelevant.
        let weights: serde_json::Map<String, Value> = artifact["profile"]["families"]
            .as_object()
            .unwrap()
            .keys()
            .map(|family| {
                (
                    family.clone(),
                    json!(if family == "rhythm" { 1.0 } else { 0.0 }),
                )
            })
            .collect();
        controls["vector_target"]["family_weights"] = Value::Object(weights);
        write_json(&f.controls, &controls);
        f
    }

    fn command(&self, subcommand: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_unslop"));
        command
            .current_dir(self.root.path())
            .env_remove("PANGRAM_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("ANTHROPIC_API_KEY")
            .args([
                "--python",
                "/style-test-does-not-require-python",
                subcommand,
            ]);
        command
    }

    fn fit_command(&self, samples: &Path, profile: &Path) -> Command {
        let mut command = self.command("style-fit");
        command
            .arg(samples)
            .args(["--author", AUTHOR, "--register", REGISTER, "--out"])
            .arg(profile);
        command
    }

    fn controls_command(&self, profile: &Path, out: &Path, rhetoric: &str) -> Command {
        let mut command = self.command("style-controls");
        command
            .arg(profile)
            .args([
                "--accessibility",
                "0",
                "--rhetorical-strength",
                rhetoric,
                "--out",
            ])
            .arg(out);
        command
    }

    fn proposal(
        &self,
        label: &str,
        profile: &Path,
        controls: &Path,
        text: &str,
    ) -> (PathBuf, Value) {
        let plan_path = self.root.path().join(format!("{label}-plan.jsonl"));
        success(
            self.command("style-plan")
                .arg(profile)
                .arg(&self.source)
                .arg("--target")
                .arg(controls)
                .arg("--out")
                .arg(&plan_path)
                .args(["--candidates", "1", "--max-edit-ratio", "0.5"])
                .output()
                .unwrap(),
        );
        let prompt = read_jsonl(&plan_path).unwrap().remove(0);
        let candidate_path = self.root.path().join(format!("{label}-collected.jsonl"));
        // collect returns identity inside metadata.prompt_record, not a top-level id.
        let collected = json!({"text":text,"corpus":"synthetic-style-proposals",
            "source_kind":"model","authorship":"synthetic_fixture","author_id":AUTHOR,
            "provider":"synthetic_fixture","model":"synthetic_fixture",
            "register":REGISTER,"group_id":prompt["group_id"],"split":prompt["split"],
            "metadata":{"prompt_record":prompt,"text_sha256":digest(text),"fixture":true}});
        assert!(collected.get("id").is_none());
        write_jsonl(&candidate_path, &[collected]);
        (candidate_path, prompt)
    }

    fn rank_command(
        &self,
        profile: &Path,
        controls: &Path,
        candidates: &Path,
        reviews: Option<&Path>,
        label: &str,
    ) -> Command {
        let mut command = self.command("style-rank");
        command
            .arg(profile)
            .arg(&self.source)
            .arg(candidates)
            .arg("--target")
            .arg(controls)
            .arg("--out")
            .arg(self.root.path().join(format!("{label}-rank.json")))
            .args(["--max-edit-ratio", "0.5"]);
        if let Some(path) = reviews {
            command.arg("--reviews").arg(path);
        }
        command
    }

    fn review(&self, report: &Value, label: &str) -> PathBuf {
        let mut review = report["candidates"][0]["review_template"].clone();
        for field in [
            "meaning_preserved",
            "details_preserved",
            "tone_preserved",
            "readable",
            "controls_satisfied",
        ] {
            review[field] = json!(true);
        }
        review["review_note"] = json!("Synthetic-fixture assistant attestation for testing only.");
        let path = self.root.path().join(format!("{label}-review.jsonl"));
        write_jsonl(&path, &[review]);
        path
    }
}

#[test]
fn offline_plan_binds_exact_inputs_and_unreviewed_improvement_never_recommends() {
    let f = Fixture::new(SOURCE);
    let (candidates, prompt) = f.proposal("bound", &f.profile, &f.controls, CANDIDATE);
    assert_eq!(prompt["source_sha256"], digest(SOURCE));
    assert_eq!(
        prompt["profile_sha256"],
        digest(&serde_json::to_string(&read_json(&f.profile)).unwrap())
    );
    assert_eq!(
        prompt["controls_sha256"],
        digest(&serde_json::to_string(&read_json(&f.controls)).unwrap())
    );
    let report = success(
        f.rank_command(&f.profile, &f.controls, &candidates, None, "unreviewed")
            .output()
            .unwrap(),
    );
    let candidate = &report["candidates"][0];
    assert_eq!(candidate["id"], prompt["id"]);
    assert_eq!(candidate["proposal_binding"], "bound_to_plan");
    assert_eq!(candidate["style_improves"], true);
    assert_eq!(candidate["eligible_for_review"], true);
    assert_eq!(candidate["recommended_after_review"], false);
    assert!(report["recommended_candidate_id"].is_null());
    assert_eq!(fs::read_to_string(&f.source).unwrap(), SOURCE);
}

#[test]
fn exact_assistant_review_can_accept_an_observed_style_improvement() {
    let f = Fixture::new(SOURCE);
    let (candidates, prompt) = f.proposal("reviewed", &f.profile, &f.controls, CANDIDATE);
    let unreviewed = success(
        f.rank_command(&f.profile, &f.controls, &candidates, None, "before-review")
            .output()
            .unwrap(),
    );
    let reviews = f.review(&unreviewed, "exact");
    let report = success(
        f.rank_command(
            &f.profile,
            &f.controls,
            &candidates,
            Some(&reviews),
            "after-review",
        )
        .output()
        .unwrap(),
    );
    assert_eq!(report["recommended_candidate_id"], prompt["id"]);
    assert_eq!(report["candidates"][0]["recommended_after_review"], true);
    assert_eq!(
        report["candidates"][0]["review"]["reviewed_by_human"],
        false
    );
    assert_eq!(report["meaning_preservation_established_by_metric"], false);
}

#[test]
fn collected_candidate_provenance_cannot_survive_changed_text_or_identity() {
    let f = Fixture::new(SOURCE);
    let (candidates, _) = f.proposal("tampered", &f.profile, &f.controls, CANDIDATE);
    let mut row = read_jsonl(&candidates).unwrap().remove(0);
    row["text"] = json!("A different text.");
    write_jsonl(&candidates, &[row.clone()]);
    failure(
        f.rank_command(&f.profile, &f.controls, &candidates, None, "changed-text")
            .output()
            .unwrap(),
        "text differs from its recorded hash",
    );
    row["text"] = json!(CANDIDATE);
    row["id"] = json!("invented-prompt-id");
    write_jsonl(&candidates, &[row]);
    failure(
        f.rank_command(&f.profile, &f.controls, &candidates, None, "changed-id")
            .output()
            .unwrap(),
        "id differs from its collected prompt id",
    );
}

#[test]
fn reviews_do_not_transfer_to_a_changed_control_goal_or_writer_profile() {
    for change_profile in [false, true] {
        let f = Fixture::new(SOURCE);
        let (candidates, _) = f.proposal("old", &f.profile, &f.controls, CANDIDATE);
        let old = success(
            f.rank_command(&f.profile, &f.controls, &candidates, None, "old")
                .output()
                .unwrap(),
        );
        let review = f.review(&old, "old");
        let new_profile = if change_profile {
            let samples = f.root.path().join("other-samples.jsonl");
            write_jsonl(
                &samples,
                &[
                    sample(
                        "other-a",
                        "other-work-a",
                        "train",
                        "A nurse measured each sample. A colleague verified every value.",
                    ),
                    sample(
                        "other-b",
                        "other-work-b",
                        "train",
                        "A keeper fed each animal. A visitor watched every movement.",
                    ),
                ],
            );
            let profile = f.root.path().join("other-profile.json");
            success(f.fit_command(&samples, &profile).output().unwrap());
            profile
        } else {
            f.profile.clone()
        };
        let new_controls = f.root.path().join("other-controls.json");
        success(
            f.controls_command(&new_profile, &new_controls, "1")
                .output()
                .unwrap(),
        );
        let (new_candidates, _) = f.proposal("new", &new_profile, &new_controls, CANDIDATE);
        failure(
            f.rank_command(
                &new_profile,
                &new_controls,
                &new_candidates,
                Some(&review),
                "stale-review",
            )
            .output()
            .unwrap(),
            "Review belongs to a different profile or control objective",
        );
        assert!(!f.root.path().join("stale-review-rank.json").exists());
    }
}

#[test]
fn changed_numeric_values_and_signs_are_ineligible_even_with_a_positive_review() {
    for value in ["21", "-20", "−20"] {
        let f = Fixture::new(SOURCE);
        let changed = CANDIDATE.replace("20", value);
        let (candidates, _) = f.proposal("numeric", &f.profile, &f.controls, &changed);
        let unreviewed = success(
            f.rank_command(&f.profile, &f.controls, &candidates, None, "numeric-before")
                .output()
                .unwrap(),
        );
        assert_eq!(unreviewed["candidates"][0]["style_improves"], true);
        assert_eq!(
            unreviewed["candidates"][0]["numeric_sequence_preserved"], false,
            "Numeric mutation {value}"
        );
        let reviews = f.review(&unreviewed, "numeric");
        let reviewed = success(
            f.rank_command(
                &f.profile,
                &f.controls,
                &candidates,
                Some(&reviews),
                "numeric-after",
            )
            .output()
            .unwrap(),
        );
        assert_eq!(reviewed["candidates"][0]["eligible_for_review"], false);
        assert_eq!(reviewed["candidates"][0]["recommended_after_review"], false);
        assert!(reviewed["recommended_candidate_id"].is_null());
    }
}

#[test]
fn exact_reference_copy_is_flagged_despite_improved_distance_and_preserved_numbers() {
    let f = Fixture::new("The clerk filed each report, and the manager checked every page.");
    let (candidates, _) = f.proposal("copy", &f.profile, &f.controls, REFERENCE_A);
    let report = success(
        f.rank_command(&f.profile, &f.controls, &candidates, None, "copy")
            .output()
            .unwrap(),
    );
    let candidate = &report["candidates"][0];
    assert_eq!(candidate["style_improves"], true);
    assert_eq!(candidate["within_edit_budget"], true);
    assert_eq!(candidate["numeric_sequence_preserved"], true);
    assert_eq!(candidate["candidate_reference_overlap"], true);
    assert_eq!(candidate["eligible_for_review"], false);
    assert!(report["recommended_candidate_id"].is_null());
}

#[test]
fn exact_sample_content_and_source_groups_cannot_cross_heldout_splits() {
    for copy_text in [false, true] {
        let f = Fixture::new(SOURCE);
        let leaked = f.root.path().join("leaked-samples.jsonl");
        let heldout = if copy_text {
            sample("heldout", "different-work", "test", REFERENCE_A)
        } else {
            sample(
                "heldout",
                "work-a",
                "dev",
                "A distinct held-out passage from the same source work.",
            )
        };
        write_jsonl(
            &leaked,
            &[
                sample("sample-a", "work-a", "train", REFERENCE_A),
                sample("sample-b", "work-b", "train", REFERENCE_B),
                heldout,
            ],
        );
        let out = f.root.path().join("leaked-profile.json");
        failure(
            f.fit_command(&leaked, &out).output().unwrap(),
            "crosses training/held-out splits",
        );
        assert!(!out.exists());
    }
}

#[test]
fn a_frozen_profile_replays_identical_input_and_refuses_changed_samples() {
    let f = Fixture::new(SOURCE);
    let original = fs::read(&f.profile).unwrap();
    success(f.fit_command(&f.samples, &f.profile).output().unwrap());
    assert_eq!(fs::read(&f.profile).unwrap(), original);
    write_jsonl(
        &f.samples,
        &[
            sample(
                "sample-a",
                "work-a",
                "train",
                "The clerk carefully filed each report. The manager checked every page.",
            ),
            sample("sample-b", "work-b", "train", REFERENCE_B),
        ],
    );
    failure(
        f.fit_command(&f.samples, &f.profile).output().unwrap(),
        "Output already differs",
    );
    assert_eq!(fs::read(&f.profile).unwrap(), original);
}

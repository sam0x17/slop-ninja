//! Artifact checks.
//!
//! [`SpecCheck`] runs the admission checks of the *A* artifact spec through
//! the artifact-check crate (`slop_ninja_artifact_check`) on the object in
//! place: the six-file table, the tokenizer's pinned digest and length, the
//! role schemas and bindings (the export manifest's digests compared with the
//! header's), the ONNX pre-parse (custom domains, external data, operator
//! allow-list), the graph class and the parameter bound. It hashes no file:
//! every digest it needs is the header's, which `pack` computed while it
//! wrote the files (the graph allow-list's structure digest is the one
//! remaining exception, design decision D5). A file that changed on disk
//! since `pack` is not caught here: R2 refuses it at upload (`BadDigest`).
//! A refusal's code is the one the coordinator reports in `check_failed`'s
//! `detail.check`.
//!
//! [`StructuralCheck`] adds what the cohort announces: its output contract and
//! its object bound. (An object whose header does not decode never gets this
//! far.)
//!
//! When the cohort names another artifact spec than the one compiled in, the
//! result is [`Outcome::Incomplete`]: this build cannot speak for that spec.
//!
//! [`ProvenanceCheck`] warns about JSON strings and keys that read as or
//! carry a local path ([`crate::provenance`]): the JSON files are published
//! when the cohort's scoring starts. Its warnings never refuse and never
//! leave a check incomplete, and a scan that cannot run is a note.

use std::path::Path;

use crate::object::{ObjectHeader, role_name};
use crate::provenance::Scan;
use anyhow::Result;
use serde::Serialize;
use slop_ninja_admission_wire::json::ArtifactSpecRef;
use slop_ninja_artifact_check::Version;

/// What a check reads.
#[derive(Clone, Copy, Debug)]
pub struct CheckContext<'a> {
    pub object: &'a Path,
    /// The object's header, read without hashing any file.
    pub header: &'a ObjectHeader,
    /// The cohort's output contract, when known.
    pub expected_contract: Option<&'a str>,
    /// The cohort's `max_object_len`, when known.
    pub max_object_len: Option<u64>,
    /// The cohort's artifact spec, when known.
    pub artifact_spec: Option<&'a ArtifactSpecRef>,
    /// Why the cohort is unknown when a coordinator was named but could not
    /// be read (None: no coordinator was given).
    pub cohort_unknown: Option<&'a str>,
    /// The next cohort whose plan gave the values above, when no cohort is
    /// admitting (None: the admitting cohort's, or none).
    pub next_cohort: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Every check ran and passed.
    Pass,
    /// A check refused the artifact.
    Refused,
    /// Nothing refused it, but some admission checks did not run.
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// Stable code; for admission checks, the code the coordinator reports in
    /// `check_failed`'s `detail.check`.
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub message: String,
    /// Whether this finding refuses the artifact.
    pub refuses: bool,
    /// A warning: something to fix before the artifact is published, which
    /// never refuses it. Serialized only when set.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub warning: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckReport {
    pub checker: String,
    pub outcome: Outcome,
    pub findings: Vec<Finding>,
}

/// One set of artifact checks.
pub trait ArtifactCheck {
    /// Checker identity and version, for example `structural/v1`.
    fn id(&self) -> &str;
    fn check(&self, cx: &CheckContext<'_>) -> Result<CheckReport>;
}

fn note(code: &str, message: String) -> Finding {
    Finding {
        code: code.into(),
        file: None,
        message,
        refuses: false,
        warning: false,
    }
}

fn warning(code: &str, file: &str, message: String) -> Finding {
    Finding {
        file: Some(file.into()),
        warning: true,
        ..note(code, message)
    }
}

fn refusal(code: &str, message: String) -> Finding {
    Finding {
        refuses: true,
        ..note(code, message)
    }
}

fn outcome(findings: &[Finding], complete: bool) -> Outcome {
    if findings.iter().any(|f| f.refuses) {
        Outcome::Refused
    } else if complete {
        Outcome::Pass
    } else {
        Outcome::Incomplete
    }
}

/// The cohort's output contract and object bound.
pub struct StructuralCheck;

pub const STRUCTURAL_ID: &str = "slop_ninja_miner.structural/v1";

impl ArtifactCheck for StructuralCheck {
    fn id(&self) -> &str {
        STRUCTURAL_ID
    }

    fn check(&self, cx: &CheckContext<'_>) -> Result<CheckReport> {
        let mut findings = Vec::new();
        let header = &cx.header.header;
        if let Some(expected) = cx.expected_contract
            && header.contract != expected
        {
            findings.push(refusal(
                "contract_mismatch",
                format!(
                    "the header names contract {:?}; the cohort expects {expected:?}",
                    header.contract
                ),
            ));
        }
        if let Some(max) = cx.max_object_len
            && cx.header.object_len > max
        {
            findings.push(refusal(
                "object_too_large",
                format!(
                    "the object is {} bytes; the cohort allows {max}",
                    cx.header.object_len
                ),
            ));
        }
        if let Some(next) = cx.next_cohort {
            findings.push(note(
                "next_cohort",
                format!(
                    "no cohort is admitting: the contract and object bound checked are cohort {next}'s, \
                     the next to open; check again once it admits"
                ),
            ));
        }
        let complete = cx.expected_contract.is_some() && cx.max_object_len.is_some();
        if !complete {
            let why = cx.cohort_unknown.unwrap_or(
                "no coordinator given (--coordinator, --public, or a trust file that pins one origin)",
            );
            findings.push(note(
                "cohort_unknown",
                format!("{why}: the cohort's contract and object bound were not checked"),
            ));
        }
        Ok(CheckReport {
            checker: STRUCTURAL_ID.into(),
            outcome: outcome(&findings, complete),
            findings,
        })
    }
}

/// The admission checks of an artifact spec this build defines: the version the
/// cohort names (by id and digest), or, with no cohort, the newest.
pub struct SpecCheck;

impl ArtifactCheck for SpecCheck {
    fn id(&self) -> &str {
        Version::LATEST.id()
    }

    fn check(&self, cx: &CheckContext<'_>) -> Result<CheckReport> {
        let mut findings = Vec::new();
        let mut complete = true;
        let named = cx
            .artifact_spec
            .and_then(|c| Version::named(&c.id, &c.sha256));
        let version = named.unwrap_or(Version::LATEST);
        match cx.artifact_spec {
            Some(cohort) if named.is_none() => {
                complete = false;
                let built: Vec<String> = Version::ALL
                    .iter()
                    .map(|v| format!("{} ({})", v.id(), v.spec_sha256()))
                    .collect();
                findings.push(note(
                    "artifact_spec_mismatch",
                    format!(
                        "the cohort checks artifact spec {} ({}); this build checks {}. \
                         Update slop_ninja_miner",
                        cohort.id,
                        cohort.sha256,
                        built.join(", ")
                    ),
                ));
            }
            Some(_) => {}
            None => findings.push(note(
                "artifact_spec_assumed",
                format!(
                    "checked against this build's newest artifact spec {} ({})",
                    version.id(),
                    version.spec_sha256()
                ),
            )),
        }
        match slop_ninja_artifact_check::fs::check_object_for(cx.object, version) {
            Ok(report) => {
                if report.content_sha256 != cx.header.content.hex() {
                    findings.push(refusal(
                        "object_changed",
                        "the object's header changed while it was checked".into(),
                    ));
                }
                findings.push(note(
                    "admitted_by_spec",
                    format!(
                        "graph class {}, {} parameters, profile {}",
                        report.graph_class, report.parameters, report.profile_id
                    ),
                ));
            }
            Err(refused) => findings.push(refusal(refused.code(), refused.to_string())),
        }
        Ok(CheckReport {
            checker: version.id().into(),
            outcome: outcome(&findings, complete),
            findings,
        })
    }
}

/// JSON strings and keys that read as or carry a local path (absolute, a
/// home directory or file URL in text, or a path under a private tree):
/// warnings only.
pub struct ProvenanceCheck;

pub const PROVENANCE_ID: &str = "slop_ninja_miner.provenance/v1";

impl ArtifactCheck for ProvenanceCheck {
    fn id(&self) -> &str {
        PROVENANCE_ID
    }

    fn check(&self, cx: &CheckContext<'_>) -> Result<CheckReport> {
        let mut findings = Vec::new();
        let not_scanned = |name: &str, why: String| Finding {
            file: Some(name.into()),
            ..note(
                "not_scanned",
                format!("{name}: not scanned for local paths: {why}"),
            )
        };
        // A scan that cannot run is a note: this checker never refuses and
        // never makes a check incomplete.
        let scans = match crate::provenance::scan_object(cx.object, cx.header) {
            Ok(scans) => scans,
            Err(error) => {
                findings.push(note(
                    "not_scanned",
                    format!("not scanned for local paths: {error:#}"),
                ));
                Vec::new()
            }
        };
        for scan in scans {
            match scan.scan {
                Scan::Paths(hits) if hits.is_empty() => {}
                Scan::Paths(hits) => findings.push(warning(
                    "absolute_path",
                    &scan.name,
                    crate::provenance::advice(&scan.name, &hits),
                )),
                Scan::NotJson(error) | Scan::Unreadable(error) => {
                    findings.push(not_scanned(&scan.name, error))
                }
                Scan::OverBudget(len) => findings.push(not_scanned(
                    &scan.name,
                    format!("{len} bytes, over its role's runtime bound"),
                )),
            }
        }
        Ok(CheckReport {
            checker: PROVENANCE_ID.into(),
            outcome: Outcome::Pass,
            findings,
        })
    }
}

/// Runs every checker. The combined outcome is `Refused` if any refused,
/// `Incomplete` if any was incomplete, and `Pass` only when all passed.
pub fn run_all(
    checks: &[&dyn ArtifactCheck],
    cx: &CheckContext<'_>,
) -> Result<(Outcome, Vec<CheckReport>)> {
    let mut reports = Vec::with_capacity(checks.len());
    for check in checks {
        reports.push(check.check(cx)?);
    }
    let outcome = if reports.iter().any(|r| r.outcome == Outcome::Refused) {
        Outcome::Refused
    } else if reports.is_empty() || reports.iter().any(|r| r.outcome == Outcome::Incomplete) {
        Outcome::Incomplete
    } else {
        Outcome::Pass
    };
    Ok((outcome, reports))
}

/// Lists the object's files with their roles, for reports.
pub fn file_lines(object: &ObjectHeader) -> Vec<String> {
    object
        .header
        .files
        .iter()
        .map(|f| format!("{:<20} {} {} bytes", role_name(f.role), f.name, f.len))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_artifact_check::spec;
    use slop_ninja_protocol::{FileRole, artifact_object};

    /// The header of an object held in memory (test only).
    fn header_of(object: &[u8]) -> ObjectHeader {
        let len = u32::from_le_bytes(object[..4].try_into().unwrap()) as usize;
        ObjectHeader::from_header_bytes(object[4..4 + len].to_vec()).unwrap()
    }

    fn verified(files: &[(&str, FileRole, &[u8])]) -> ObjectHeader {
        header_of(&artifact_object("c-1", files).unwrap())
    }

    fn run(v: &ObjectHeader, contract: Option<&str>, max: Option<u64>) -> CheckReport {
        let cx = CheckContext {
            object: Path::new("unused"),
            header: v,
            expected_contract: contract,
            max_object_len: max,
            artifact_spec: None,
            cohort_unknown: None,
            next_cohort: None,
        };
        StructuralCheck.check(&cx).unwrap()
    }

    #[test]
    fn structure_passes_only_with_the_cohort_known() {
        let v = verified(&[("m", FileRole::Graph, b"g")]);
        let report = run(&v, Some("c-1"), Some(v.object_len));
        assert_eq!(report.outcome, Outcome::Pass);
        assert_eq!(run(&v, None, None).outcome, Outcome::Incomplete);
    }

    #[test]
    fn contract_and_size_refuse() {
        let v = verified(&[("m", FileRole::Graph, b"g")]);
        let codes = |r: CheckReport| -> Vec<String> {
            r.findings
                .into_iter()
                .filter(|f| f.refuses)
                .map(|f| f.code)
                .collect()
        };
        assert_eq!(codes(run(&v, Some("c-2"), None)), ["contract_mismatch"]);
        assert_eq!(
            codes(run(&v, None, Some(v.object_len - 1))),
            ["object_too_large"]
        );
    }

    #[test]
    fn the_spec_check_refuses_with_the_artifact_check_code() {
        let dir = tempfile::tempdir().unwrap();
        let object =
            artifact_object(spec::CONTRACT_ID, &[("model.onnx", FileRole::Graph, b"g")]).unwrap();
        let path = dir.path().join("o");
        std::fs::write(&path, &object).unwrap();
        let v = header_of(&object);
        let mut cx = CheckContext {
            object: &path,
            header: &v,
            expected_contract: None,
            max_object_len: None,
            artifact_spec: None,
            cohort_unknown: None,
            next_cohort: None,
        };
        let report = SpecCheck.check(&cx).unwrap();
        assert_eq!(report.outcome, Outcome::Refused);
        let refused: Vec<&str> = report
            .findings
            .iter()
            .filter(|f| f.refuses)
            .map(|f| f.code.as_str())
            .collect();
        assert_eq!(refused, ["undeclared_file"]);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "artifact_spec_assumed")
        );
        // Another cohort spec: the refusal stands; nothing could pass.
        let other = ArtifactSpecRef {
            id: "slop_ninja_a_artifact_spec_v2".into(),
            sha256: "00".repeat(32),
        };
        cx.artifact_spec = Some(&other);
        let report = SpecCheck.check(&cx).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "artifact_spec_mismatch" && !f.refuses)
        );
    }

    #[test]
    fn absolute_paths_warn_without_refusing() {
        let dir = tempfile::tempdir().unwrap();
        let contract =
            br#"{"runs":[{"run":"/opt/elsewhere/run-000"},{"run":"/opt/elsewhere/run-001"}]}"#;
        let object = artifact_object(
            spec::CONTRACT_ID,
            &[
                ("a-fp32band.onnx", FileRole::Graph, b"g"),
                ("contract.json", FileRole::OutputSchema, contract),
                (
                    "manifest.json",
                    FileRole::OwnerMetadata,
                    br#"{"fit_dir":"fits/one"}"#,
                ),
            ],
        )
        .unwrap();
        let path = dir.path().join("o");
        std::fs::write(&path, &object).unwrap();
        let v = header_of(&object);
        let cx = CheckContext {
            object: &path,
            header: &v,
            expected_contract: None,
            max_object_len: None,
            artifact_spec: None,
            cohort_unknown: None,
            next_cohort: None,
        };
        let report = ProvenanceCheck.check(&cx).unwrap();
        assert_eq!(report.outcome, Outcome::Pass);
        assert_eq!(report.findings.len(), 1);
        let f = &report.findings[0];
        assert!(f.warning && !f.refuses && f.code == "absolute_path");
        assert_eq!(f.file.as_deref(), Some("contract.json"));
        assert!(
            f.message
                .contains("2 strings or keys read as or carry a local path (/runs/*/run)"),
            "{}",
            f.message
        );
        assert!(
            !f.message.contains("/opt/elsewhere"),
            "the report names pointers, not paths"
        );
        // Only a warning is serialized with its flag.
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["findings"][0]["warning"], true);
        assert!(
            serde_json::to_value(note("x", String::new()))
                .unwrap()
                .get("warning")
                .is_none()
        );
        // With the spec check, the combined outcome is the spec check's.
        let (outcome, reports) = run_all(&[&SpecCheck, &ProvenanceCheck], &cx).unwrap();
        assert_eq!(outcome, Outcome::Refused);
        assert_eq!(reports[1].outcome, Outcome::Pass);
        // An object that cannot be scanned is a note, never an error or a
        // refusal: the check still passes.
        std::fs::write(&path, &object[..object.len() - 1]).unwrap();
        let report = ProvenanceCheck.check(&cx).unwrap();
        assert_eq!(report.outcome, Outcome::Pass);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].code == "not_scanned" && !report.findings[0].refuses);
    }

    #[test]
    fn a_different_cohort_spec_makes_a_passing_check_incomplete() {
        let findings = vec![note("artifact_spec_mismatch", String::new())];
        assert_eq!(outcome(&findings, false), Outcome::Incomplete);
        assert_eq!(outcome(&[], true), Outcome::Pass);
    }

    #[test]
    fn combined_outcome() {
        struct Fixed(Outcome);
        impl ArtifactCheck for Fixed {
            fn id(&self) -> &str {
                "fixed"
            }
            fn check(&self, _: &CheckContext<'_>) -> Result<CheckReport> {
                Ok(CheckReport {
                    checker: "fixed".into(),
                    outcome: self.0,
                    findings: vec![],
                })
            }
        }
        let v = verified(&[("m", FileRole::Graph, b"g")]);
        let cx = CheckContext {
            object: Path::new("unused"),
            header: &v,
            expected_contract: None,
            max_object_len: None,
            artifact_spec: None,
            cohort_unknown: None,
            next_cohort: None,
        };
        let (pass, refused, incomplete) = (
            Fixed(Outcome::Pass),
            Fixed(Outcome::Refused),
            Fixed(Outcome::Incomplete),
        );
        assert_eq!(run_all(&[&pass, &pass], &cx).unwrap().0, Outcome::Pass);
        assert_eq!(
            run_all(&[&pass, &incomplete], &cx).unwrap().0,
            Outcome::Incomplete
        );
        assert_eq!(
            run_all(&[&incomplete, &refused], &cx).unwrap().0,
            Outcome::Refused
        );
        assert_eq!(run_all(&[], &cx).unwrap().0, Outcome::Incomplete);
    }
}

//! Exact, reversible-source patches. No parser or model is needed to apply them.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub start_byte: usize,
    pub end_byte: usize,
    pub expected: String,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeaningStatus {
    RequiresReview,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub rule: String,
    pub rule_version: String,
    pub catalog_version: String,
    pub parser_identity: String,
    pub source_sha256: String,
    pub candidate_sha256: String,
    pub text: String,
    pub edits: Vec<Edit>,
    pub preconditions: Vec<String>,
    pub meaning_status: MeaningStatus,
    pub semantic_equivalence_certified: bool,
    pub risks: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct RuleIdentity<'a> {
    pub rule: &'a str,
    pub rule_version: &'a str,
    pub catalog_version: &'a str,
    pub parser_identity: &'a str,
}

pub fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Validate every original byte span before constructing any output.
pub fn apply(text: &str, edits: &[Edit]) -> Result<String> {
    let mut sorted: Vec<_> = edits.iter().collect();
    sorted.sort_by_key(|edit| (edit.start_byte, edit.end_byte));
    for (index, edit) in sorted.iter().enumerate() {
        ensure!(
            edit.start_byte <= edit.end_byte
                && edit.end_byte <= text.len()
                && text.is_char_boundary(edit.start_byte)
                && text.is_char_boundary(edit.end_byte),
            "Edit offsets must bound an original UTF-8 substring"
        );
        ensure!(
            text[edit.start_byte..edit.end_byte] == edit.expected,
            "Edit expected text does not match the original byte span"
        );
        if index > 0 {
            let previous = sorted[index - 1];
            ensure!(
                previous.end_byte <= edit.start_byte && previous.start_byte != edit.start_byte,
                "Edits overlap or have an ambiguous shared insertion position"
            );
        }
    }
    let mut output = String::new();
    let mut cursor = 0;
    for edit in sorted {
        output.push_str(&text[cursor..edit.start_byte]);
        output.push_str(&edit.replacement);
        cursor = edit.end_byte;
    }
    output.push_str(&text[cursor..]);
    Ok(output)
}

pub fn candidate(
    text: &str,
    identity: RuleIdentity<'_>,
    mut edits: Vec<Edit>,
    preconditions: &[&str],
    risks: &[&str],
) -> Result<Candidate> {
    ensure!(
        [
            identity.rule,
            identity.rule_version,
            identity.catalog_version,
            identity.parser_identity
        ]
        .iter()
        .all(|value| !value.trim().is_empty())
            && !edits.is_empty(),
        "A candidate needs a rule and edits"
    );
    let revised = apply(text, &edits)?;
    ensure!(revised != text, "A candidate must change the source");
    edits.sort_by_key(|edit| (edit.start_byte, edit.end_byte));
    let candidate_sha256 = digest(&revised);
    Ok(Candidate {
        id: format!("{}-{}", identity.rule, &candidate_sha256[..16]),
        rule: identity.rule.into(),
        rule_version: identity.rule_version.into(),
        catalog_version: identity.catalog_version.into(),
        parser_identity: identity.parser_identity.into(),
        source_sha256: digest(text),
        candidate_sha256,
        text: revised,
        edits,
        preconditions: preconditions.iter().map(|value| (*value).into()).collect(),
        meaning_status: MeaningStatus::RequiresReview,
        semantic_equivalence_certified: false,
        risks: risks.iter().map(|value| (*value).into()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> RuleIdentity<'static> {
        RuleIdentity {
            rule: "fixture",
            rule_version: "fixture-v1",
            catalog_version: "fixture-catalog-v1",
            parser_identity: "fixture-parser-v1",
        }
    }

    #[test]
    fn exact_unicode_byte_patches_preserve_untouched_source() {
        let source = "Cafe\u{301} isn’t closed.";
        let start = source.find("isn’t").unwrap();
        let edit = Edit {
            start_byte: start,
            end_byte: start + "isn’t".len(),
            expected: "isn’t".into(),
            replacement: "is not".into(),
        };
        let result = candidate(
            source,
            identity(),
            vec![edit.clone()],
            &["fixture"],
            &["review tone"],
        )
        .unwrap();
        assert_eq!(result.text, "Cafe\u{301} is not closed.");
        assert_eq!(source, "Cafe\u{301} isn’t closed.");
        assert_eq!(result.source_sha256, digest(source));
        assert_eq!(result.candidate_sha256, digest(&result.text));
        assert_eq!(apply(source, &[edit]).unwrap(), result.text);
        assert_eq!(result.meaning_status, MeaningStatus::RequiresReview);
        assert!(!result.semantic_equivalence_certified);
    }

    #[test]
    fn malformed_spans_overlaps_and_stale_expectations_fail() {
        let edit = |start, end, expected: &str| Edit {
            start_byte: start,
            end_byte: end,
            expected: expected.into(),
            replacement: "x".into(),
        };
        assert!(apply("éabc", &[edit(1, 2, "")]).is_err());
        assert!(apply("éabc", &[edit(2, 9, "abc")]).is_err());
        assert!(apply("éabc", &[edit(2, 3, "b")]).is_err());
        assert!(apply("éabc", &[edit(2, 4, "ab"), edit(3, 5, "bc")]).is_err());
        assert!(apply("éabc", &[edit(2, 2, ""), edit(2, 3, "a")]).is_err());
        assert!(
            candidate(
                "abc",
                identity(),
                vec![Edit {
                    start_byte: 0,
                    end_byte: 1,
                    expected: "a".into(),
                    replacement: "a".into()
                }],
                &[],
                &[]
            )
            .is_err()
        );
        assert_eq!(
            apply("abc", &[edit(2, 3, "c"), edit(0, 1, "a")]).unwrap(),
            "xbx"
        );
    }
}

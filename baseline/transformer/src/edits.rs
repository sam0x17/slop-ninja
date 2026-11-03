//! Source-span edits with explicit mechanical limits. Editorial review is separate.
use anyhow::{Context, Result, ensure};
use grammar_core::features;
use serde::{Deserialize, Serialize};

pub const MAX_SOURCE_PATCHES: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Output {
    Prose,
    SourceEdits {
        max_edits: usize,
        max_word_edit_fraction: f64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Edit {
    pub before: String,
    pub after: String,
    /// Exact UTF-8 byte position in the source. Without it, the anchor must be unique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_byte: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    edits: Vec<Edit>,
}

#[derive(Serialize)]
pub struct Applied {
    pub text: String,
    pub edits: Vec<Edit>,
    pub word_edit_count: usize,
    pub source_words: usize,
}

pub fn word_edit_count(source: &str, candidate: &str) -> usize {
    let a = features::words(source);
    let b = features::words(candidate);
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, left) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, right) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(left != right))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()]
}

fn digits(value: &str) -> Vec<&str> {
    let mut items: Vec<_> = value
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .collect();
    items.sort_unstable();
    items
}

pub fn apply(source: &str, raw: &str, output: &Output) -> Result<Applied> {
    let source_words = features::words(source).len();
    ensure!(source_words > 0, "Empty source");
    match output {
        Output::Prose => Ok(Applied {
            text: raw.to_owned(),
            edits: vec![],
            word_edit_count: word_edit_count(source, raw),
            source_words,
        }),
        Output::SourceEdits {
            max_edits,
            max_word_edit_fraction,
        } => {
            ensure!(
                (1..=MAX_SOURCE_PATCHES).contains(max_edits)
                    && max_word_edit_fraction.is_finite()
                    && *max_word_edit_fraction > 0.0
                    && *max_word_edit_fraction <= 1.0,
                "Invalid edit limits"
            );
            let proposed: Proposal = serde_json::from_str(raw)?;
            ensure!(proposed.edits.len() <= *max_edits, "Too many edits");
            let mut spans = Vec::new();
            for edit in &proposed.edits {
                ensure!(!edit.before.is_empty(), "Empty edit anchor");
                let start = if let Some(start) = edit.start_byte {
                    start
                } else {
                    let first = source.find(&edit.before).context("Missing source anchor")?;
                    ensure!(
                        source.rfind(&edit.before) == Some(first),
                        "Anchor must occur exactly once in the source, or specify start_byte"
                    );
                    first
                };
                let end = start
                    .checked_add(edit.before.len())
                    .context("Edit span overflow")?;
                ensure!(
                    source.get(start..end) == Some(edit.before.as_str()),
                    "Edit byte span does not match the exact source text"
                );
                ensure!(
                    digits(&edit.before) == digits(&edit.after),
                    "An edit changed digit tokens"
                );
                ensure!(
                    !edit.after.chars().any(|c| {
                        (c.is_control() && c != '\n' && c != '\t')
                            || matches!(
                                c,
                                '\u{200b}'
                                    | '\u{200c}'
                                    | '\u{200d}'
                                    | '\u{2060}'
                                    | '\u{feff}'
                                    | '\u{00ad}'
                            )
                    }),
                    "Hidden/control characters are not editing candidates"
                );
                spans.push((start, end, edit));
            }
            spans.sort_by_key(|s| s.0);
            let mut text = String::new();
            let mut cursor = 0;
            for (start, end, edit) in spans {
                ensure!(start >= cursor, "Overlapping source edits");
                text.push_str(&source[cursor..start]);
                text.push_str(&edit.after);
                cursor = end;
            }
            text.push_str(&source[cursor..]);
            let word_edit_count = word_edit_count(source, &text);
            ensure!(
                word_edit_count as f64 / source_words as f64 <= *max_word_edit_fraction,
                "Word edit budget exceeded"
            );
            Ok(Applied {
                text,
                edits: proposed.edits,
                word_edit_count,
                source_words,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_patch_budget_admits_more_small_edits_without_relaxing_word_limits() {
        let source = "The lamp is lit. ".repeat(20);
        let patches: Vec<_> = source
            .match_indices('.')
            .map(|(start, _)| json!({"before":".","after":";","start_byte":start}))
            .collect();
        let raw = json!({"edits":patches}).to_string();
        let mode = |max_edits| Output::SourceEdits {
            max_edits,
            max_word_edit_fraction: 0.01,
        };
        assert!(apply(&source, &raw, &mode(16)).is_err());
        let result = apply(&source, &raw, &mode(32)).unwrap();
        assert_eq!(result.text, source.replace('.', ";"));
        assert_eq!(result.word_edit_count, 0);
        assert!(apply(&source, &raw, &mode(MAX_SOURCE_PATCHES + 1)).is_err());
        let added_words = json!({"edits":[{"before":"lamp","after":"bright green overhead lamp","start_byte":4}]}).to_string();
        assert!(apply(&source, &added_words, &mode(32)).is_err());
    }

    #[test]
    fn source_anchors_keep_unedited_conditions_exact() {
        let source = "The café opens at 9. We cannot promise an earlier opening.";
        let mode = Output::SourceEdits {
            max_edits: 2,
            max_word_edit_fraction: 0.4,
        };
        let raw = json!({"edits":[{"before":"We cannot promise", "after":"We can't promise"}]})
            .to_string();
        let result = apply(source, &raw, &mode).unwrap();
        assert_eq!(
            result.text,
            "The café opens at 9. We can't promise an earlier opening."
        );
        assert_eq!(result.word_edit_count, 1);
    }

    #[test]
    fn ambiguous_overlapping_numeric_and_insertion_edits_fail() {
        let mode = Output::SourceEdits {
            max_edits: 3,
            max_word_edit_fraction: 0.3,
        };
        for (source, proposal) in [
            (
                "We wait. We listen.",
                json!({"edits":[{"before":"We", "after":"They"}]}),
            ),
            (
                "We can wait until 9.",
                json!({"edits":[{"before":"until 9", "after":"until 10"}]}),
            ),
            (
                "We can wait until 9.",
                json!({"edits":[{"before":"We can", "after":"We may"},{"before":"can wait", "after":"can stay"}]}),
            ),
            (
                "We can wait until 9.",
                json!({"edits":[{"before":"We", "after":"We and everyone else who comes here in the morning"}]}),
            ),
        ] {
            assert!(apply(source, &proposal.to_string(), &mode).is_err());
        }
    }

    #[test]
    fn byte_anchors_select_repeated_occurrences_and_reject_stale_or_invalid_spans() {
        let source = "The café waits. The café waits.";
        let mode = Output::SourceEdits {
            max_edits: 2,
            max_word_edit_fraction: 0.5,
        };
        let first = source.find("waits").unwrap();
        let second = source.rfind("waits").unwrap();
        let raw = json!({"edits":[{"before":"waits","after":"stays","start_byte":second}]});
        assert_eq!(
            apply(source, &raw.to_string(), &mode).unwrap().text,
            "The café waits. The café stays."
        );
        let both = json!({"edits":[{"before":"waits","after":"stays","start_byte":second},
            {"before":"waits","after":"opens","start_byte":first}]});
        assert_eq!(
            apply(source, &both.to_string(), &mode).unwrap().text,
            "The café opens. The café stays."
        );
        for offset in [first - 1, second + 1, source.len(), usize::MAX] {
            let bad = json!({"edits":[{"before":"waits","after":"stays","start_byte":offset}]});
            assert!(apply(source, &bad.to_string(), &mode).is_err());
        }
        let bad_utf8 =
            json!({"edits":[{"before":"é","after":"e","start_byte":source.find('é').unwrap()+1}]});
        assert!(apply(source, &bad_utf8.to_string(), &mode).is_err());
        let overlap = json!({"edits":[{"before":"waits","after":"stays","start_byte":first},
            {"before":"waits","after":"opens","start_byte":first}]});
        assert!(apply(source, &overlap.to_string(), &mode).is_err());
        let ambiguous = json!({"edits":[{"before":"ana","after":"era"}]});
        assert!(apply("banana", &ambiguous.to_string(), &mode).is_err());
    }
}

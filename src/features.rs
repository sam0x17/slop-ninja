//! Versioned observable lexical features, with an optional Python ML parser.
//!
//! Construction counts use sentences as opportunities, so co-occurring grammar
//! patterns must never be summed to produce their denominator.

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use unicode_categories::UnicodeCategories;
use unicode_normalization::UnicodeNormalization;

pub const LEXICAL_EXTRACTOR: &str =
    "unslop-features-rust-v1;nfc-unicode-word-v1;sentence-heuristic-v1";
static SENTENCE_BOUNDARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[.!?]+[\"'”’)\]]*(?:\s+|$)|\n\s*\n"#).unwrap());
static PARAGRAPH_BOUNDARY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());

fn normalized(text: &str) -> String {
    text.nfc()
        .map(|ch| match ch {
            '\u{2018}' | '\u{2019}' | '\u{02bc}' | '\u{ff07}' => '\'',
            _ => ch,
        })
        .collect()
}

/// NFC words with letter starts, combining marks, and internal apostrophes.
/// Lowercasing preserves German ß; hyphens, digits, and underscores separate.
pub fn words(text: &str) -> Vec<String> {
    let text = normalized(text);
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    let mut current = String::new();
    while let Some(ch) = chars.next() {
        let apostrophe =
            ch == '\'' && !current.is_empty() && chars.peek().is_some_and(|next| next.is_letter());
        if ch.is_letter() || (ch.is_mark() && !current.is_empty()) || apostrophe {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(current.to_lowercase().nfc().collect());
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(current.to_lowercase().nfc().collect());
    }
    tokens
}

type Counts = BTreeMap<String, u64>;

fn counts(tokens: impl IntoIterator<Item = String>) -> Counts {
    let mut counts = Counts::new();
    for token in tokens {
        *counts.entry(token).or_default() += 1;
    }
    counts
}

fn ngrams(sentences: &[Vec<String>], size: usize) -> Counts {
    counts(
        sentences
            .iter()
            .flat_map(|sentence| sentence.windows(size).map(|ngram| ngram.join(" "))),
    )
}

fn length_bucket(length: usize) -> &'static str {
    match length {
        0..10 => "01-09",
        10..20 => "10-19",
        20..30 => "20-29",
        30..40 => "30-39",
        _ => "40+",
    }
}

fn ratio(numerator: f64, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator / denominator as f64
    }
}

pub fn extract(text: &str, grammar: bool, python: &str) -> Result<Value> {
    extract_with_model(text, grammar, python, "en_core_web_sm")
}

pub fn extract_with_model(text: &str, grammar: bool, python: &str, model: &str) -> Result<Value> {
    let tokens = words(text);
    let sentences: Vec<Vec<String>> = SENTENCE_BOUNDARY
        .split(text)
        .map(words)
        .filter(|tokens| !tokens.is_empty())
        .collect();
    let mut lengths: Vec<usize> = sentences.iter().map(Vec::len).collect();
    let word_counts = counts(tokens.iter().cloned());
    let distinct_count = word_counts.len();
    let word_lengths: Vec<usize> = tokens
        .iter()
        .map(|token| token.chars().filter(|ch| ch.is_letter()).count())
        .collect();
    let mut families = Map::new();
    families.insert("word".into(), json!(word_counts));
    families.insert("word_bigram".into(), json!(ngrams(&sentences, 2)));
    families.insert("word_trigram".into(), json!(ngrams(&sentences, 3)));
    families.insert(
        "sentence_length".into(),
        json!(counts(
            lengths
                .iter()
                .map(|length| length_bucket(*length).to_string())
        )),
    );
    let mut totals: Map<String, Value> = families
        .iter()
        .map(|(family, values)| {
            (
                family.clone(),
                json!(
                    values
                        .as_object()
                        .unwrap()
                        .values()
                        .map(|v| v.as_u64().unwrap())
                        .sum::<u64>()
                ),
            )
        })
        .collect();
    let mean_sentence_words = ratio(lengths.iter().sum::<usize>() as f64, lengths.len());
    let sentence_variance = ratio(
        lengths
            .iter()
            .map(|&length| (length as f64 - mean_sentence_words).powi(2))
            .sum(),
        lengths.len(),
    );
    lengths.sort_unstable();
    let p90 = if lengths.is_empty() {
        0
    } else {
        lengths[(9 * lengths.len()).div_ceil(10) - 1]
    };
    let mut metrics = json!({
        "word_count": tokens.len(), "distinct_word_count": distinct_count,
        "type_token_ratio": ratio(distinct_count as f64, tokens.len()),
        "mean_word_letters": ratio(word_lengths.iter().sum::<usize>() as f64, tokens.len()),
        "long_word_fraction_ge7": ratio(word_lengths.iter().filter(|&&length| length >= 7).count() as f64, tokens.len()),
        "heuristic_sentence_count": lengths.len(),
        "heuristic_mean_sentence_words": mean_sentence_words,
        "heuristic_sentence_words_stdev": sentence_variance.sqrt(),
        "heuristic_sentence_words_p90": p90,
        "paragraph_count": PARAGRAPH_BOUNDARY.split(text).filter(|part| !words(part).is_empty()).count(),
    }).as_object().unwrap().clone();
    let mut identity = LEXICAL_EXTRACTOR.to_string();
    if grammar {
        let parsed = parse_grammar(text, python, model)?;
        let parser_identity = parsed["extractor"]
            .as_str()
            .context("Grammar bridge omitted extractor identity")?;
        if parser_identity.is_empty() {
            bail!("Grammar bridge returned empty extractor identity");
        }
        identity.push(';');
        identity.push_str(parser_identity);
        for (target, field) in [
            (&mut families, "families"),
            (&mut totals, "totals"),
            (&mut metrics, "metrics"),
        ] {
            let source = parsed[field]
                .as_object()
                .with_context(|| format!("Grammar bridge omitted {field}"))?;
            for (key, value) in source {
                if target.contains_key(key) {
                    bail!("Grammar bridge attempted to overwrite lexical {field}: {key}");
                }
                target.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(json!({"extractor": identity, "families":families, "totals":totals, "metrics":metrics}))
}

fn parse_grammar(text: &str, python: &str, model: &str) -> Result<Value> {
    let bridge = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("python/nlp_bridge.py");
    let mut child = Command::new(python)
        .arg(&bridge)
        .arg("--model")
        .arg(model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Cannot start grammar interpreter {python:?}"))?;
    // The bridge reads stdin before running its model, then emits one JSON object.
    let input_result = child
        .stdin
        .take()
        .context("Grammar bridge stdin unavailable")?
        .write_all(text.as_bytes());
    let output = child
        .wait_with_output()
        .context("Cannot wait for grammar bridge")?;
    if !output.status.success() {
        bail!(
            "Grammar extraction failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    input_result.context("Cannot write text to grammar bridge")?;
    serde_json::from_slice(&output.stdout).context("Grammar bridge returned invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lexical(text: &str) -> Value {
        extract(text, false, "no-parser-required").unwrap()
    }

    #[test]
    fn unicode_words_preserve_combining_marks_and_internal_apostrophes() {
        assert_eq!(
            words("CAFÉ cafe\u{301} DON’T don't l’esprit l'esprit co-operate 123 under_score"),
            [
                "café", "café", "don't", "don't", "l'esprit", "l'esprit", "co", "operate", "under",
                "score"
            ]
        );
        assert_eq!(words("हिन्दी 中文 Straße"), ["हिन्दी", "中文", "straße"]);
        assert_eq!(
            words("'hello' rock ‘n’ roll"),
            ["hello", "rock", "n", "roll"]
        );
    }

    #[test]
    fn canonical_equivalence_preserves_all_metrics() {
        assert_eq!(
            lexical("CAFÉ isn't ordinary. CAFÉ matters."),
            lexical("cafe\u{301} isn’t ordinary. café matters.")
        );
    }

    #[test]
    fn ngrams_obey_sentence_and_paragraph_boundaries() {
        let result = lexical("One two three. \"Four five!\"\n\nSix seven eight nine");
        assert_eq!(result["totals"]["word"], 9);
        assert_eq!(result["totals"]["word_bigram"], 6);
        assert_eq!(result["totals"]["word_trigram"], 3);
        assert!(
            result["families"]["word_bigram"]
                .get("three four")
                .is_none()
        );
        assert!(result["families"]["word_bigram"].get("five six").is_none());
        assert_eq!(result["metrics"]["heuristic_sentence_count"], 3);
        assert_eq!(result["metrics"]["paragraph_count"], 2);
        for (family, counts) in result["families"].as_object().unwrap() {
            let count: u64 = counts
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_u64().unwrap())
                .sum();
            assert_eq!(result["totals"][family], count);
        }
    }

    #[test]
    fn punctuation_and_empty_inputs_have_finite_zero_metrics() {
        for text in ["", " \n\t ", "1234 ...!? --__"] {
            let result = lexical(text);
            assert!(
                result["totals"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|v| v.as_u64() == Some(0))
            );
            assert!(
                result["metrics"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|v| v.as_f64() == Some(0.0))
            );
            assert!(
                result["families"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|v| v.as_object().unwrap().is_empty())
            );
        }
    }

    #[test]
    fn paragraph_and_sentence_percentiles_are_defined() {
        let result = lexical("One. Two words. Three words here.");
        assert_eq!(result["metrics"]["heuristic_sentence_words_p90"], 3);
        assert_eq!(result["metrics"]["heuristic_mean_sentence_words"], 2.0);
        assert!(
            (result["metrics"]["heuristic_sentence_words_stdev"]
                .as_f64()
                .unwrap()
                - (2_f64 / 3.0).sqrt())
            .abs()
                < 1e-12
        );
    }

    #[test]
    fn grammar_failure_is_not_silently_ignored() {
        assert!(
            extract("Some text.", true, "/unslop-test-nonexistent-python")
                .unwrap_err()
                .to_string()
                .contains("Cannot start grammar interpreter")
        );
    }
}

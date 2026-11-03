//! Observable style coordinates without a detector-derived preferred direction.
//!
//! Closed word lists and punctuation categories are versioned surface heuristics.
//! They do not establish grammatical function, rhetorical meaning, or quality.

use crate::features;
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use unicode_categories::UnicodeCategories;

const STYLE_VERSION: &str = "unslop-style-v1";
const WORD_LENGTH_SCALE_FLOOR: f64 = 0.25;
const WORD_COUNT_SCALE_FLOOR: f64 = 2.0;
const FRACTION_SCALE_FLOOR: f64 = 0.02;

const FUNCTION_WORDS: &[&str] = &[
    "a",
    "about",
    "above",
    "across",
    "after",
    "against",
    "all",
    "along",
    "although",
    "am",
    "among",
    "an",
    "and",
    "another",
    "any",
    "are",
    "around",
    "as",
    "at",
    "be",
    "because",
    "been",
    "before",
    "behind",
    "being",
    "below",
    "beneath",
    "beside",
    "between",
    "beyond",
    "both",
    "but",
    "by",
    "can",
    "could",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "during",
    "each",
    "either",
    "every",
    "except",
    "few",
    "for",
    "from",
    "had",
    "has",
    "have",
    "having",
    "he",
    "her",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "i",
    "if",
    "in",
    "inside",
    "into",
    "is",
    "it",
    "its",
    "itself",
    "many",
    "may",
    "me",
    "might",
    "mine",
    "much",
    "must",
    "my",
    "myself",
    "near",
    "neither",
    "no",
    "nor",
    "not",
    "of",
    "off",
    "on",
    "onto",
    "or",
    "other",
    "our",
    "ours",
    "ourselves",
    "out",
    "outside",
    "over",
    "several",
    "shall",
    "she",
    "should",
    "since",
    "so",
    "some",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "these",
    "they",
    "this",
    "those",
    "though",
    "through",
    "to",
    "toward",
    "towards",
    "under",
    "unless",
    "until",
    "up",
    "upon",
    "us",
    "was",
    "we",
    "were",
    "what",
    "whatever",
    "when",
    "where",
    "whereas",
    "whether",
    "which",
    "while",
    "who",
    "whoever",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "within",
    "without",
    "would",
    "yet",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

// Match these literal word sequences through the existing sentence-bounded
// unigram/bigram/trigram counts. An occurrence need not act as a discourse marker.
const DISCOURSE_SURFACES: &[&str] = &[
    "accordingly",
    "also",
    "as a result",
    "by contrast",
    "consequently",
    "finally",
    "first",
    "for example",
    "for instance",
    "furthermore",
    "however",
    "in addition",
    "in conclusion",
    "in contrast",
    "in fact",
    "in other words",
    "in particular",
    "in short",
    "indeed",
    "instead",
    "likewise",
    "meanwhile",
    "moreover",
    "nevertheless",
    "nonetheless",
    "of course",
    "otherwise",
    "overall",
    "second",
    "similarly",
    "so",
    "that is",
    "then",
    "therefore",
    "thus",
    "to begin with",
    "yet",
];

const CONSTRUCTIONS: &[&str] = &[
    "adverbial_clause_sentence",
    "apposition_sentence",
    "complement_clause_sentence",
    "coordination_sentence",
    "copular_sentence",
    "existential_there_sentence",
    "first_person_reference_sentence",
    "negation_sentence",
    "noun_subject_sentence",
    "passive_sentence",
    "pronoun_subject_sentence",
    "relative_clause_sentence",
];

// A fixed spelling list avoids treating every possessive or foreign-language
// apostrophe as a contraction. It is deliberately a surface proxy, not a parse.
const CONTRACTIONS: &[&str] = &[
    "ain't",
    "aren't",
    "can't",
    "couldn't",
    "daren't",
    "didn't",
    "doesn't",
    "don't",
    "hadn't",
    "hasn't",
    "haven't",
    "isn't",
    "mayn't",
    "mightn't",
    "mustn't",
    "needn't",
    "oughtn't",
    "shan't",
    "shouldn't",
    "wasn't",
    "weren't",
    "won't",
    "wouldn't",
    "i'm",
    "i'd",
    "i'll",
    "i've",
    "you're",
    "you'd",
    "you'll",
    "you've",
    "he's",
    "he'd",
    "he'll",
    "she's",
    "she'd",
    "she'll",
    "it's",
    "it'd",
    "it'll",
    "we're",
    "we'd",
    "we'll",
    "we've",
    "they're",
    "they'd",
    "they'll",
    "they've",
    "that's",
    "that'd",
    "that'll",
    "there's",
    "there'd",
    "there'll",
    "there're",
    "there've",
    "here's",
    "here'd",
    "here'll",
    "what's",
    "what'd",
    "what'll",
    "what're",
    "what've",
    "who's",
    "who'd",
    "who'll",
    "who're",
    "who've",
    "where's",
    "where'd",
    "where'll",
    "where're",
    "when's",
    "when'd",
    "when'll",
    "why's",
    "why'd",
    "why'll",
    "why're",
    "how's",
    "how'd",
    "how'll",
    "how're",
    "let's",
];

const PUNCTUATION: &[&str] = &[
    "all_punctuation",
    "apostrophe_or_single_quote",
    "brace",
    "colon",
    "comma",
    "double_quote",
    "ellipsis",
    "em_dash",
    "en_dash",
    "exclamation",
    "guillemet",
    "hyphen",
    "other_punctuation",
    "parenthesis",
    "period",
    "question",
    "semicolon",
    "slash",
    "square_bracket",
];

fn total(base: &Value, family: &str) -> Result<u64> {
    base["totals"][family]
        .as_u64()
        .with_context(|| format!("Missing {family} opportunity count"))
}

fn counts<'a>(base: &'a Value, family: &str) -> Result<&'a Map<String, Value>> {
    base["families"][family]
        .as_object()
        .with_context(|| format!("Missing {family} counts"))
}

fn observed(counts: &Map<String, Value>, key: &str) -> Result<u64> {
    counts.get(key).map_or(Ok(0), |value| {
        value.as_u64().context("Invalid underlying feature count")
    })
}

fn rate(count: u64, opportunities: u64) -> f64 {
    if opportunities == 0 {
        0.0
    } else {
        count as f64 / opportunities as f64
    }
}

fn distribution(base: &Value, family: &str) -> Result<Value> {
    let opportunities = total(base, family)?;
    let mut values = Map::new();
    let mut sum = 0_u64;
    for (key, value) in counts(base, family)? {
        let count = value.as_u64().context("Invalid distribution count")?;
        ensure!(
            count <= opportunities,
            "Distribution count exceeds opportunities"
        );
        sum = sum
            .checked_add(count)
            .context("Distribution count overflow")?;
        if opportunities > 0 {
            values.insert(key.clone(), json!(rate(count, opportunities)));
        }
    }
    ensure!(
        sum == opportunities,
        "Distribution counts must sum to opportunities"
    );
    Ok(json!({"kind":"distribution","opportunities":opportunities,"values":values}))
}

fn fixed_rates(base: &Value, family: &str, catalog: &[&str], opportunities: u64) -> Result<Value> {
    let counts = counts(base, family)?;
    let mut values = Map::new();
    for key in catalog {
        let count = observed(counts, key)?;
        ensure!(count <= opportunities, "Rate count exceeds opportunities");
        values.insert((*key).to_string(), json!(rate(count, opportunities)));
    }
    Ok(json!({"kind":"rates","opportunities":opportunities,"values":values}))
}

fn punctuation_kind(ch: char) -> Option<&'static str> {
    match ch {
        '.' | '．' | '。' => Some("period"),
        ',' | '，' | '、' => Some("comma"),
        ';' | '；' | '؛' => Some("semicolon"),
        ':' | '：' => Some("colon"),
        '!' | '！' | '¡' => Some("exclamation"),
        '?' | '？' | '¿' | '؟' => Some("question"),
        '-' | '‐' | '‑' | '﹣' | '－' => Some("hyphen"),
        '–' => Some("en_dash"),
        '—' => Some("em_dash"),
        '…' => Some("ellipsis"),
        '\'' | '‘' | '’' | '‚' | '‛' | 'ʼ' | '＇' => Some("apostrophe_or_single_quote"),
        '"' | '“' | '”' | '„' | '‟' | '＂' => Some("double_quote"),
        '«' | '»' | '‹' | '›' => Some("guillemet"),
        '(' | ')' | '（' | '）' => Some("parenthesis"),
        '[' | ']' | '［' | '］' => Some("square_bracket"),
        '{' | '}' | '｛' | '｝' => Some("brace"),
        '/' | '\\' | '／' | '＼' => Some("slash"),
        _ => None,
    }
}

fn punctuation(text: &str) -> Value {
    let mut counts: BTreeMap<&str, u64> = PUNCTUATION.iter().map(|&key| (key, 0)).collect();
    let mut opportunities = 0;
    for ch in text.chars() {
        opportunities += 1;
        if ch.is_punctuation() {
            *counts.get_mut("all_punctuation").unwrap() += 1;
        }
        if let Some(kind) = punctuation_kind(ch) {
            *counts.get_mut(kind).unwrap() += 1;
        } else if ch.is_punctuation() {
            *counts.get_mut("other_punctuation").unwrap() += 1;
        }
    }
    let values: BTreeMap<_, _> = counts
        .into_iter()
        .map(|(key, count)| (key, rate(count, opportunities)))
        .collect();
    json!({"kind":"rates","opportunities":opportunities,"values":values})
}

fn metric(base: &Value, name: &str) -> Result<f64> {
    let value = base["metrics"][name]
        .as_f64()
        .with_context(|| format!("Missing metric {name}"))?;
    ensure!(
        value.is_finite() && value >= 0.0,
        "Invalid nonnegative metric {name}"
    );
    Ok(value)
}

fn build(text: &str, grammar: bool, base: &Value) -> Result<Value> {
    let words = total(base, "word")?;
    let paragraphs = base["metrics"]["paragraph_count"]
        .as_u64()
        .context("Missing paragraph count")?;
    let identity = base["extractor"]
        .as_str()
        .context("Missing underlying extractor identity")?;
    let mut families = Map::new();
    families.insert("lexicon".into(), distribution(base, "word")?);
    families.insert(
        "function_word".into(),
        fixed_rates(base, "word", FUNCTION_WORDS, words)?,
    );
    families.insert(
        "sentence_length".into(),
        distribution(base, "sentence_length")?,
    );
    families.insert("punctuation".into(), punctuation(text));
    let mut discourse = Map::new();
    for surface in DISCOURSE_SURFACES {
        let family = match surface.split_whitespace().count() {
            1 => "word",
            2 => "word_bigram",
            3 => "word_trigram",
            _ => unreachable!("The v1 discourse catalog contains at most three words per phrase"),
        };
        let count = observed(counts(base, family)?, surface)?;
        ensure!(count <= words, "Surface phrase count exceeds lexical words");
        discourse.insert((*surface).to_string(), json!(rate(count, words)));
    }
    families.insert(
        "discourse_surface".into(),
        json!({"kind":"rates","opportunities":words,"values":discourse}),
    );
    families.insert("rhythm".into(), json!({
        "kind":"metrics","opportunities":words,
        "values":{
            "mean_sentence_words":metric(base,"heuristic_mean_sentence_words")?,
            "sentence_words_stdev":metric(base,"heuristic_sentence_words_stdev")?,
            "sentence_words_p90":metric(base,"heuristic_sentence_words_p90")?,
            "words_per_paragraph":rate(words,paragraphs),
        },
        "scale_floors":{
            "mean_sentence_words":WORD_COUNT_SCALE_FLOOR,"sentence_words_stdev":WORD_COUNT_SCALE_FLOOR,
            "sentence_words_p90":WORD_COUNT_SCALE_FLOOR,"words_per_paragraph":WORD_COUNT_SCALE_FLOOR,
        },
    }));
    let lexical_counts = counts(base, "word")?;
    let contraction_count = CONTRACTIONS.iter().try_fold(0_u64, |sum, word| {
        sum.checked_add(observed(lexical_counts, word)?)
            .context("Contraction count overflow")
    })?;
    ensure!(
        contraction_count <= words,
        "Contraction count exceeds lexical words"
    );
    families.insert("lexical_shape".into(),json!({
        "kind":"metrics","opportunities":words,
        "values":{
            "mean_word_letters":metric(base,"mean_word_letters")?,
            "long_word_fraction_ge7":metric(base,"long_word_fraction_ge7")?,
            "contraction_fraction":rate(contraction_count,words),
        },
        "scale_floors":{
            "mean_word_letters":WORD_LENGTH_SCALE_FLOOR,
            "long_word_fraction_ge7":FRACTION_SCALE_FLOOR,"contraction_fraction":FRACTION_SCALE_FLOOR,
        },
    }));
    if grammar {
        for family in [
            "pos",
            "dependency",
            "pos_bigram",
            "pos_trigram",
            "function_pattern",
        ] {
            families.insert(family.to_string(), distribution(base, family)?);
        }
        families.insert(
            "construction".into(),
            fixed_rates(
                base,
                "construction",
                CONSTRUCTIONS,
                total(base, "construction")?,
            )?,
        );
    }
    Ok(json!({
        "extractor":format!("{STYLE_VERSION};{identity}"),"families":families,
        "diagnostics":{
            "word_count":words,"unicode_character_count":text.chars().count(),"paragraph_count":paragraphs,
            "heuristic_sentence_count":base["metrics"]["heuristic_sentence_count"],
            "parsed_sentence_count":if grammar {base["metrics"]["parsed_sentence_count"].clone()}else{Value::Null},
            "parsed_token_count":if grammar {base["metrics"]["parsed_token_count"].clone()}else{Value::Null},
            "grammar_enabled":grammar,"underlying_extractor":identity,
            "surface_catalog_version":"english-and-unicode-surface-v1",
            "scale_floor_version":"unslop-style-v1",
            "opportunity_units":{
                "lexicon":"lexical_words","function_word":"all_lexical_words","discourse_surface":"all_lexical_words",
                "sentence_length":"heuristic_nonempty_sentences","punctuation":"unicode_scalar_values",
                "rhythm":"lexical_words; individual metrics use heuristic sentences or nonempty paragraphs",
                "lexical_shape":"lexical_words","construction":"parsed_nonempty_sentences",
                "pos":"parsed_lexical_tokens","dependency":"parsed_lexical_tokens",
                "pos_bigram":"parsed_sentence_bigrams","pos_trigram":"parsed_sentence_trigrams",
                "function_pattern":"parsed_sentence_function_pattern_trigrams",
            },
            "interpretation":"Function-word and discourse lists count literal normalized spellings, without inferring grammatical function or rhetorical meaning. Contraction fraction uses a fixed English spelling list. Punctuation uses original Unicode scalars; all_punctuation overlaps the named classes, and modifier-letter apostrophes may be named without belonging to Unicode punctuation. Grammar patterns describe the parser's analysis. No coordinate specifies a preferred style or detector outcome.",
        },
    }))
}

pub fn extract(text: &str, grammar: bool, python: &str) -> Result<Value> {
    let base = features::extract(text, grammar, python)?;
    build(text, grammar, &base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lexical(text: &str) -> Value {
        extract(text, false, "/not-needed-python").unwrap()
    }

    #[test]
    fn every_distribution_and_rate_uses_its_declared_denominator() {
        let result = lexical("And and however. For example, however.");
        assert_eq!(result["diagnostics"]["word_count"], 6);
        assert_eq!(result["families"]["function_word"]["opportunities"], 6);
        assert!(
            (result["families"]["function_word"]["values"]["and"]
                .as_f64()
                .unwrap()
                - 2.0 / 6.0)
                .abs()
                < 1e-12
        );
        assert!(
            (result["families"]["discourse_surface"]["values"]["however"]
                .as_f64()
                .unwrap()
                - 2.0 / 6.0)
                .abs()
                < 1e-12
        );
        assert!(
            (result["families"]["discourse_surface"]["values"]["for example"]
                .as_f64()
                .unwrap()
                - 1.0 / 6.0)
                .abs()
                < 1e-12
        );
        assert_eq!(result["families"]["sentence_length"]["opportunities"], 2);
        for family in result["families"].as_object().unwrap().values() {
            let values: Vec<_> = family["values"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_f64().unwrap())
                .collect();
            assert!(values.iter().all(|v| v.is_finite() && *v >= 0.0));
            if family["kind"] == "distribution" {
                assert!((values.iter().sum::<f64>() - 1.0).abs() < 1e-12);
            }
            if family["kind"] == "rates" {
                assert!(values.iter().all(|v| *v <= 1.0));
            }
        }
    }

    #[test]
    fn punctuation_uses_unicode_scalars_not_utf8_bytes() {
        let result = lexical("“Hi”—go… 😃!");
        let punctuation = &result["families"]["punctuation"];
        assert_eq!(punctuation["opportunities"], 11);
        assert_eq!(punctuation["values"]["double_quote"], 2.0 / 11.0);
        assert_eq!(punctuation["values"]["em_dash"], 1.0 / 11.0);
        assert_eq!(punctuation["values"]["ellipsis"], 1.0 / 11.0);
        assert_eq!(punctuation["values"]["all_punctuation"], 5.0 / 11.0);
        assert_eq!(punctuation["values"]["semicolon"], 0.0);
    }

    #[test]
    fn absent_fixed_surfaces_remain_zero_and_phrases_never_cross_sentences() {
        let result = lexical("For. Example. In\n\nfact.");
        assert_eq!(
            result["families"]["function_word"]["values"]["although"],
            0.0
        );
        assert_eq!(
            result["families"]["discourse_surface"]["values"]["for example"],
            0.0
        );
        assert_eq!(
            result["families"]["discourse_surface"]["values"]["in fact"],
            0.0
        );
        assert!(result["families"].get("construction").is_none());
        assert!(result["diagnostics"]["parsed_sentence_count"].is_null());
    }

    #[test]
    fn empty_text_has_no_invented_observations_and_finite_zero_values() {
        let result = lexical("");
        for family in result["families"].as_object().unwrap().values() {
            assert_eq!(family["opportunities"], 0);
            assert!(
                family["values"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|v| v.as_f64() == Some(0.0))
            );
            if family["kind"] == "metrics" {
                assert_eq!(
                    family["values"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<Vec<_>>(),
                    family["scale_floors"]
                        .as_object()
                        .unwrap()
                        .keys()
                        .collect::<Vec<_>>()
                );
                assert!(
                    family["scale_floors"]
                        .as_object()
                        .unwrap()
                        .values()
                        .all(|v| v.as_f64().unwrap() > 0.0)
                );
            }
        }
        let punctuation_only = lexical("…");
        assert_eq!(punctuation_only["families"]["lexicon"]["opportunities"], 0);
        assert_eq!(
            punctuation_only["families"]["punctuation"]["opportunities"],
            1
        );
        assert_eq!(
            punctuation_only["families"]["punctuation"]["values"]["ellipsis"],
            1.0
        );
    }

    #[test]
    fn contraction_spellings_are_normalized_without_counting_every_apostrophe() {
        let result = lexical("I don’t read John's l'esprit.");
        assert_eq!(result["diagnostics"]["word_count"], 5);
        assert_eq!(
            result["families"]["lexical_shape"]["values"]["contraction_fraction"],
            0.2
        );
        assert_eq!(
            result["families"]["lexical_shape"]["scale_floors"]["contraction_fraction"],
            0.02
        );
        assert_eq!(
            result["families"]["lexical_shape"]["scale_floors"]["mean_word_letters"],
            0.25
        );
        assert_eq!(
            result["families"]["rhythm"]["scale_floors"]["words_per_paragraph"],
            2.0
        );
    }

    #[test]
    fn grammar_absence_is_an_error_and_lexical_mode_does_not_load_it() {
        assert!(
            extract("Some text.", true, "/unslop-nonexistent-parser-python")
                .unwrap_err()
                .to_string()
                .contains("Cannot start grammar interpreter")
        );
        let result = lexical("Some text.");
        let base = features::extract("Some text.", false, "/not-needed-python").unwrap();
        assert_eq!(
            result["extractor"],
            format!("unslop-style-v1;{}", base["extractor"].as_str().unwrap())
        );
    }

    #[test]
    fn installed_grammar_keeps_lexical_denominators_and_zero_patterns() {
        let python = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".venv/bin/python");
        if !python.exists() {
            return;
        } // The repository does not require the optional ML environment.
        let result = extract("I don't agree.", true, python.to_str().unwrap()).unwrap();
        let lexical = lexical("I don't agree.");
        assert_eq!(
            result["families"]["lexicon"],
            lexical["families"]["lexicon"]
        );
        assert_eq!(
            result["families"]["function_word"],
            lexical["families"]["function_word"]
        );
        assert_eq!(result["diagnostics"]["word_count"], 3);
        assert_eq!(result["families"]["pos"]["opportunities"], 4);
        assert_eq!(result["families"]["construction"]["opportunities"], 1);
        assert_eq!(
            result["families"]["construction"]["values"]["first_person_reference_sentence"],
            1.0
        );
        assert_eq!(
            result["families"]["construction"]["values"]["passive_sentence"],
            0.0
        );
        assert_eq!(
            result["families"]["construction"]["values"]
                .as_object()
                .unwrap()
                .len(),
            CONSTRUCTIONS.len()
        );
        assert!(result["extractor"].as_str().unwrap().contains(";spacy="));
        for family in [
            "pos",
            "dependency",
            "pos_bigram",
            "pos_trigram",
            "function_pattern",
        ] {
            let sum: f64 = result["families"][family]["values"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_f64().unwrap())
                .sum();
            assert!((sum - 1.0).abs() < 1e-12);
        }
    }
}

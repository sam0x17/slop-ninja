//! Stylometric control (arm C): raw function-word and character 3-gram counts.
//! The 3-gram vocabulary, the log1p transform and standardization are fitted
//! later on training authors, as the frozen protocol requires.
use crate::common::*;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, collections::HashMap, path::Path, time::Instant};

/// Maximal runs of Unicode alphabetic characters, lowercased.
pub fn word_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphabetic() {
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Lowercased text with each maximal Unicode whitespace run replaced by one
/// U+0020 (no trimming).
pub fn normalized_for_ngrams(text: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut in_space = false;
    for c in text.to_lowercase().chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

pub fn char3_counts(text: &str) -> (u64, BTreeMap<String, u32>) {
    let chars = normalized_for_ngrams(text);
    let mut counts = BTreeMap::new();
    let mut total = 0u64;
    for w in chars.windows(3) {
        *counts.entry(w.iter().collect::<String>()).or_default() += 1;
        total += 1;
    }
    (total, counts)
}

pub fn function_word_counts(words: &[String], function_words: &[String]) -> Vec<u32> {
    let index: HashMap<&str, usize> = function_words
        .iter()
        .enumerate()
        .map(|(i, w)| (w.as_str(), i))
        .collect();
    let mut counts = vec![0u32; function_words.len()];
    for w in words {
        if let Some(&i) = index.get(w.as_str()) {
            counts[i] += 1;
        }
    }
    counts
}

pub fn run(frozen_path: &Path, inputs: &Path, out: &Path) -> Result<()> {
    let frozen = read_frozen(frozen_path)?;
    let function_words: Vec<String> = frozen["arms"]["C"]["function_words"]
        .as_array()
        .context("function words")?
        .iter()
        .map(|v| v.as_str().map(String::from).context("function word"))
        .collect::<Result<_>>()?;
    ensure!(function_words.len() == 203, "203 frozen function words");
    create_new_dir(out)?;
    let start = Instant::now();
    let mut lines = HashedLines::open(inputs)?;
    let mut writer = JsonlWriter::create(&out.join("stylometric-features.jsonl"))?;
    let (mut words_total, mut grams_total, mut empty_words) = (0u64, 0u64, 0u64);
    while let Some(line) = lines.next_line()? {
        if line.is_empty() {
            continue;
        }
        let req: Request = serde_json::from_slice(line)?;
        let words = word_tokens(&req.text);
        let fw = function_word_counts(&words, &function_words);
        let (total, grams) = char3_counts(&req.text);
        words_total += words.len() as u64;
        grams_total += total;
        empty_words += words.is_empty() as u64;
        let grams: Vec<(String, u32)> = grams.into_iter().collect();
        writer.write(&json!({"id":req.id,"text_sha256":sha_hex(req.text.as_bytes()),
            "word_tokens":words.len(),"function_word_counts":fw,"char3_total":total,"char3_counts":grams}))?;
    }
    let input_sha = lines.finish();
    let (rows, sha) = writer.finish()?;
    let seconds = start.elapsed().as_secs_f64();
    let manifest: Value = json!({
        "schema":"slop_ninja_cfg_vs_encoder_feature_manifest_v1","arm":"C","protocol_id":PROTOCOL_ID,
        "frozen_sha256":FROZEN_SHA256,"created":now_utc(),
        "inputs":{"requests":inputs.display().to_string(),"sha256":input_sha},
        "rules":{"word_tokens":frozen["arms"]["C"]["word_tokens"],"char_ngrams":frozen["arms"]["C"]["char_ngrams"]["text"],
            "trim":"none; a leading or trailing whitespace run becomes one space",
            "function_words_sha256":sha_hex(serde_json::to_string(&function_words)?.as_bytes()),
            "stored":"raw counts; vocabulary, transform and standardization fitted later on training authors"},
        "outputs":{"features":{"file":"stylometric-features.jsonl","rows":rows,"sha256":sha}},
        "coverage":{"documents":rows,"documents_without_word_tokens":empty_words,"word_tokens":words_total,"char3_total":grams_total},
        "runtime":{"wall_seconds":seconds,"threads":1},
        "metrics_computed":false
    });
    write_new_json(&out.join("manifest.json"), &manifest)?;
    println!("{}", serde_json::to_string(&json!({"documents":rows,"seconds":seconds}))?);
    Ok(())
}

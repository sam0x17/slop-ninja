use crate::{features::words, util::digest};
use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use unicode_normalization::UnicodeNormalization;

fn word_boundary(c: char) -> bool {
    c.is_alphanumeric() || "_'’‘ʼ＇".contains(c)
}

pub fn substitute(text: &str, old: &str, new: &str, occurrence: usize) -> Result<(String, Value)> {
    ensure!(occurrence > 0, "Occurrence is 1-based");
    for value in [old, new] {
        let normalized: String = value
            .nfc()
            .map(|c| if "’‘ʼ＇".contains(c) { '\'' } else { c })
            .collect::<String>()
            .to_lowercase()
            .nfc()
            .collect();
        ensure!(
            !value.is_empty() && words(value) == vec![normalized],
            "Single-word experiments require one complete lexical word on each side"
        );
    }
    let matches: Vec<_> = text
        .match_indices(old)
        .filter(|(i, _)| {
            !text[..*i].chars().next_back().is_some_and(word_boundary)
                && !text[*i + old.len()..]
                    .chars()
                    .next()
                    .is_some_and(word_boundary)
        })
        .collect();
    let Some((start, _)) = matches.get(occurrence - 1) else {
        bail!("Only {} exact whole-word occurrences", matches.len())
    };
    let end = start + old.len();
    let candidate = format!("{}{}{}", &text[..*start], new, &text[end..]);
    ensure!(candidate != text, "Perturbation does not change text");
    let manifest = json!({"kind":"single_word","old":old,"new":new,"occurrence":occurrence,
        "start_byte":start,"end_byte":end,"start":text[..*start].chars().count(),
        "end":text[..end].chars().count(),"source_sha256":digest(text),"candidate_sha256":digest(&candidate)});
    Ok((candidate, manifest))
}

pub fn compare_features(a_text: &str, b_text: &str, a: &Value, b: &Value) -> Result<Value> {
    ensure!(
        a["extractor"] == b["extractor"],
        "Feature comparison requires identical extractors"
    );
    let af = a["families"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Missing feature families"))?;
    let bf = b["families"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Missing feature families"))?;
    let mut deltas = Map::new();
    for family in af.keys().chain(bf.keys()).collect::<BTreeSet<_>>() {
        let empty = Map::new();
        let ac = af.get(family).and_then(Value::as_object).unwrap_or(&empty);
        let bc = bf.get(family).and_then(Value::as_object).unwrap_or(&empty);
        let mut delta = Map::new();
        for key in ac.keys().chain(bc.keys()).collect::<BTreeSet<_>>() {
            let n = bc.get(key).and_then(Value::as_i64).unwrap_or(0)
                - ac.get(key).and_then(Value::as_i64).unwrap_or(0);
            if n != 0 {
                delta.insert(key.clone(), json!(n));
            }
        }
        deltas.insert(family.clone(), json!(delta));
    }
    Ok(
        json!({"source_sha256":digest(a_text),"candidate_sha256":digest(b_text),"extractor":a["extractor"],
        "feature_count_deltas":deltas,"source_totals":a["totals"],"candidate_totals":b["totals"],
        "source_metrics":a["metrics"],"candidate_metrics":b["metrics"],
        "quality":"Feature changes cannot certify preservation, tone, or readability. Review the source and candidate."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_word_unicode_offsets() {
        let (s, m) = substitute(
            "Éva acts accordingly, then accordingly.",
            "accordingly",
            "thus",
            2,
        )
        .unwrap();
        assert_eq!(s, "Éva acts accordingly, then thus.");
        assert_eq!(
            m["start_byte"].as_u64().unwrap(),
            m["start"].as_u64().unwrap() + 1
        );
    }
    #[test]
    fn rejects_nonwords_and_contraction_fragments() {
        for s in ["...", "42", "two-words", ""] {
            assert!(substitute("We act accordingly.", "accordingly", s, 1).is_err());
        }
        assert!(substitute("It isn't.", "is", "was", 1).is_err());
    }
}

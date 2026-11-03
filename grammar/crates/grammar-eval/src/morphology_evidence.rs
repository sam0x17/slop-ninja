//! Three-valued lexical/morphological evidence, with one explicit role/form inference.
use crate::lexical_context::{lexical, normalized_lemma};
use anyhow::{Result, ensure};
use grammar_core::syntax::Token;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const SCHEMA: &str = "slopninja-morphology-evidence-v1";
const OBLIQUES: &[&str] = &["me", "us", "you", "him", "her", "it", "them"];

/// The role flag must come from exact same-member opposite-frame role and
/// attachment checks, not merely a surface pronoun or dependency label.
pub fn compare(
    source: &Token,
    candidate: &Token,
    bound_singleton_recipient: bool,
) -> Result<Value> {
    ensure!(
        source.text == candidate.text,
        "Morphology comparison requires matched token bytes"
    );
    let a = normalized_lemma(source);
    let b = normalized_lemma(candidate);
    let mut changed = Vec::new();
    for (field, diff) in [
        ("pos", source.pos != candidate.pos),
        ("tag", source.tag != candidate.tag),
        ("is_punct", source.is_punct != candidate.is_punct),
        ("is_space", source.is_space != candidate.is_space),
    ] {
        if diff {
            changed.push(field);
        }
    }
    if matches!((&a,&b),(Some(a),Some(b)) if a!=b) {
        changed.push("normalized_lemma");
    }
    let missing_lemma = (lexical(source) || lexical(candidate)) && (a.is_none() || b.is_none());
    let form = source.text.to_lowercase();
    let supported = bound_singleton_recipient
        && OBLIQUES.contains(&form.as_str())
        && source.pos == "PRON"
        && candidate.pos == "PRON"
        && source.tag == "PRP"
        && candidate.tag == "PRP";
    let keys = source
        .morph
        .keys()
        .chain(candidate.morph.keys())
        .map(String::as_str)
        .chain(supported.then_some("Case"))
        .collect::<BTreeSet<_>>();
    let mut fields = Vec::new();
    for key in keys {
        let left = source.morph.get(key);
        let right = candidate.morph.get(key);
        let (outcome, reason) = if key == "Case" && supported {
            let compatible =
                |value: Option<&Vec<String>>| value.is_none_or(|v| v.len() == 1 && v[0] == "Acc");
            if !compatible(left) || !compatible(right) {
                (
                    "changed",
                    "supplied_case_conflicts_with_supported_oblique_recipient",
                )
            } else if left != right {
                (
                    "observed_preserved",
                    "compatible_from_bound_recipient_and_closed_oblique_form",
                )
            } else if left.is_none() {
                (
                    "observed_preserved",
                    "both_case_annotations_absent_supported_role_form_retained",
                )
            } else {
                ("observed_preserved", "supplied_values_equal")
            }
        } else {
            match (left, right) {
                (Some(a), Some(b)) if a == b => ("observed_preserved", "supplied_values_equal"),
                (Some(_), Some(_)) => ("changed", "conflicting_supplied_values"),
                (None, None) => ("observed_preserved", "both_annotations_absent"),
                _ => (
                    "unresolved",
                    "one_annotation_absent_without_supported_inference",
                ),
            }
        };
        fields.push(json!({"feature":key,"source":left,"candidate":right,"outcome":outcome,"reason":reason}));
    }
    let outcome = if !changed.is_empty() || fields.iter().any(|v| v["outcome"] == "changed") {
        "changed"
    } else if missing_lemma || fields.iter().any(|v| v["outcome"] == "unresolved") {
        "unresolved"
    } else {
        "observed_preserved"
    };
    Ok(
        json!({"schema":SCHEMA,"status":"compared","outcome":outcome,
        "source_index":source.i,"candidate_index":candidate.i,"source_token":source,"candidate_token":candidate,
        "normalized_lemmas":{"source":a,"candidate":b},"changed_non_morphology_fields":changed,"missing_lexical_lemma":missing_lemma,
        "bound_singleton_recipient":bound_singleton_recipient,"supported_role_form":supported,
        "morphology_fields":fields,"annotations_modified":false,
        "semantic_equivalence_certified":false,"automatic_edit_license":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn token(text: &str, case: Option<&str>) -> Token {
        Token {
            i: 0,
            start_byte: 0,
            end_byte: text.len(),
            text: text.into(),
            lemma: text.into(),
            pos: "PRON".into(),
            tag: "PRP".into(),
            dep: "pobj".into(),
            head: 0,
            sentence: 0,
            morph: case
                .map(|c| BTreeMap::from([("Case".into(), vec![c.into()])]))
                .unwrap_or_default(),
            is_punct: false,
            is_space: false,
        }
    }
    #[test]
    fn missing_acc_needs_a_bound_supported_recipient_in_both_directions() {
        for word in OBLIQUES {
            let missing = token(word, None);
            let supplied = token(word, Some("Acc"));
            for (a, b) in [(&missing, &supplied), (&supplied, &missing)] {
                assert_eq!(
                    compare(a, b, true).unwrap()["outcome"],
                    "observed_preserved"
                );
                assert_eq!(compare(a, b, false).unwrap()["outcome"], "unresolved");
            }
            assert!(missing.morph.is_empty());
        }
    }
    #[test]
    fn supplied_conflicts_and_unsupported_forms_never_use_missing_case_exception() {
        let acc = token("her", Some("Acc"));
        let nom = token("her", Some("Nom"));
        assert_eq!(compare(&acc, &nom, true).unwrap()["outcome"], "changed");
        assert_eq!(compare(&nom, &nom, true).unwrap()["outcome"], "changed");
        for word in ["I", "they", "myself", "someone"] {
            assert_eq!(
                compare(&token(word, None), &token(word, Some("Acc")), true).unwrap()["outcome"],
                "unresolved"
            );
        }
        let mut a = token("her", None);
        let mut b = token("her", Some("Acc"));
        a.pos = "PROPN".into();
        b.pos = "PROPN".into();
        assert_eq!(compare(&a, &b, true).unwrap()["outcome"], "unresolved");
    }
    #[test]
    fn other_morphology_lexical_changes_and_unknown_lemmas_remain_failures() {
        let a = token("us", None);
        let mut b = token("us", Some("Acc"));
        b.morph.insert("Number".into(), vec!["Plur".into()]);
        assert_eq!(compare(&a, &b, true).unwrap()["outcome"], "unresolved");
        let mut a = a;
        a.morph.insert("Number".into(), vec!["Sing".into()]);
        assert_eq!(compare(&a, &b, true).unwrap()["outcome"], "changed");
        b.morph.remove("Number");
        a.morph.remove("Number");
        b.tag = "PRP$".into();
        assert_eq!(compare(&a, &b, true).unwrap()["outcome"], "changed");
        b.tag = "PRP".into();
        b.lemma = "different".into();
        assert_eq!(compare(&a, &b, true).unwrap()["outcome"], "changed");
        b.lemma = String::new();
        assert_eq!(compare(&a, &b, true).unwrap()["outcome"], "unresolved");
        b.text = "them".into();
        assert!(compare(&a, &b, true).is_err());
    }
}

//! Whole-document sentence partitions induced by an explicit token mapping.
//! Numeric sentence indices are diagnostic positions, not membership identities.
//! The caller owns rewrite authorization and the supplied carrier licenses.

use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits,
    syntax::{self, Document, Token},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-sentence-membership-evidence-v1";

struct Side {
    links: Vec<BTreeSet<usize>>,
    pairs: Vec<Vec<(usize, usize)>>,
    unlicensed: Vec<Vec<usize>>,
    licensed: Vec<Vec<usize>>,
}

fn side(doc: &Document) -> Side {
    let n = doc.sentences.len();
    Side {
        links: vec![BTreeSet::new(); n],
        pairs: vec![vec![]; n],
        unlicensed: vec![vec![]; n],
        licensed: vec![vec![]; n],
    }
}

fn evidence(tokens: &[Token], indices: &[usize]) -> Value {
    json!(indices.iter().map(|&i| &tokens[i]).collect::<Vec<_>>())
}

fn row(
    index: usize,
    source_side: bool,
    own: &Document,
    other: &Document,
    own_side: &Side,
    other_side: &Side,
) -> Value {
    let links = &own_side.links[index];
    let counterpart_multiple = links.iter().any(|&j| other_side.links[j].len() > 1);
    let own_multiple = links.len() > 1;
    let split = if source_side {
        own_multiple
    } else {
        counterpart_multiple
    };
    let merge = if source_side {
        counterpart_multiple
    } else {
        own_multiple
    };
    let one_to_one = links.len() == 1 && links.iter().all(|&j| other_side.links[j].len() == 1);
    let other_unmapped: Vec<_> = links
        .iter()
        .flat_map(|&j| {
            other_side.unlicensed[j]
                .iter()
                .map(move |&i| json!({"sentence_index":j,"token":other.tokens[i]}))
        })
        .collect();
    let mut reasons = Vec::new();
    if split {
        reasons.push("source_sentence_split_across_candidates");
    }
    if merge {
        reasons.push("candidate_sentence_merges_sources");
    }
    if own_side.pairs[index].is_empty() {
        reasons.push("sentence_has_no_mapped_tokens");
    }
    if !own_side.unlicensed[index].is_empty() {
        reasons.push("unlicensed_unmapped_tokens_in_sentence");
    }
    if !other_unmapped.is_empty() {
        reasons.push("unlicensed_unmapped_tokens_in_matched_sentence");
    }
    let outcome = if split || merge {
        "changed"
    } else if !reasons.is_empty() {
        "unresolved"
    } else {
        "observed_preserved"
    };
    let shift = if one_to_one {
        Some(index != *links.first().unwrap())
    } else {
        None
    };
    let pairs:Vec<_>=own_side.pairs[index].iter().map(|&(s,c)|{
        let (st,ct)=if source_side{(&own.tokens[s],&other.tokens[c])}else{(&other.tokens[s],&own.tokens[c])};
        json!({"source_index":s,"candidate_index":c,"source_span":[st.start_byte,st.end_byte],"candidate_span":[ct.start_byte,ct.end_byte],"text":st.text})
    }).collect();
    json!({"side":if source_side{"source"}else{"candidate"},"sentence_index":index,"sentence":own.sentences[index],
        "matched_sentence_indices":links,"mapped_token_pairs":pairs,"mapped_membership_one_to_one":one_to_one,
        "source_split":split,"candidate_merge":merge,"positional_index_shift":shift,
        "licensed_carrier_tokens":evidence(&own.tokens,&own_side.licensed[index]),
        "unlicensed_unmapped_tokens":evidence(&own.tokens,&own_side.unlicensed[index]),
        "other_side_unlicensed_unmapped_tokens":other_unmapped,"outcome":outcome,"reasons":reasons})
}

/// Compare sentence membership using mapped token occurrences and caller-supplied
/// licenses for omitted/inserted `to` carriers. This function checks mapping and
/// license bounds/disjointness; the caller must establish carrier eligibility.
/// Reordering mapped words within a sentence is supported. A split or merge
/// observed among mapped tokens is changed even if other evidence is unresolved.
pub fn compare(
    source: &Document,
    candidate: &Document,
    map: &BTreeMap<usize, usize>,
    source_omissions: &BTreeSet<usize>,
    candidate_insertions: &BTreeSet<usize>,
) -> Result<Value> {
    syntax::validate(source).context("Invalid source syntax for sentence membership")?;
    syntax::validate(candidate).context("Invalid candidate syntax for sentence membership")?;
    let mut inverse = BTreeMap::new();
    let mut s = side(source);
    let mut c = side(candidate);
    for (&si, &ci) in map {
        let st = source
            .tokens
            .get(si)
            .context("Source mapping index out of bounds")?;
        let ct = candidate
            .tokens
            .get(ci)
            .context("Candidate mapping index out of bounds")?;
        ensure!(
            inverse.insert(ci, si).is_none(),
            "Token mapping is not injective"
        );
        ensure!(
            st.text == ct.text,
            "Mapped token occurrences have different exact text"
        );
        s.links[st.sentence].insert(ct.sentence);
        c.links[ct.sentence].insert(st.sentence);
        s.pairs[st.sentence].push((si, ci));
        c.pairs[ct.sentence].push((si, ci));
    }
    for &i in source_omissions {
        let t = source
            .tokens
            .get(i)
            .context("Source omission index out of bounds")?;
        ensure!(
            !map.contains_key(&i),
            "Licensed source omission is also mapped"
        );
        s.licensed[t.sentence].push(i);
    }
    for &i in candidate_insertions {
        let t = candidate
            .tokens
            .get(i)
            .context("Candidate insertion index out of bounds")?;
        ensure!(
            !inverse.contains_key(&i),
            "Licensed candidate insertion is also mapped"
        );
        c.licensed[t.sentence].push(i);
    }
    for t in &source.tokens {
        if !map.contains_key(&t.i) && !source_omissions.contains(&t.i) {
            s.unlicensed[t.sentence].push(t.i);
        }
    }
    for t in &candidate.tokens {
        if !inverse.contains_key(&t.i) && !candidate_insertions.contains(&t.i) {
            c.unlicensed[t.sentence].push(t.i);
        }
    }
    let source_rows: Vec<_> = (0..source.sentences.len())
        .map(|i| row(i, true, source, candidate, &s, &c))
        .collect();
    let candidate_rows: Vec<_> = (0..candidate.sentences.len())
        .map(|i| row(i, false, candidate, source, &c, &s))
        .collect();
    let mut reasons = Vec::new();
    if source.sentences.is_empty() {
        reasons.push("source_has_no_sentences");
    }
    if candidate.sentences.is_empty() {
        reasons.push("candidate_has_no_sentences");
    }
    let rows = || source_rows.iter().chain(&candidate_rows);
    let changed = rows().any(|r| r["outcome"] == "changed");
    let unresolved = rows().any(|r| r["outcome"] == "unresolved") || !reasons.is_empty();
    let outcome = if changed {
        "changed"
    } else if unresolved {
        "unresolved"
    } else {
        "observed_preserved"
    };
    if changed {
        reasons.push("mapped_token_partition_split_or_merge");
    }
    if rows().any(|r| r["outcome"] == "unresolved") {
        reasons.push("incomplete_sentence_membership_evidence");
    }
    let mut source_outcomes = BTreeMap::from([
        ("observed_preserved", 0usize),
        ("changed", 0),
        ("unresolved", 0),
    ]);
    let mut candidate_outcomes = source_outcomes.clone();
    for r in &source_rows {
        *source_outcomes
            .get_mut(r["outcome"].as_str().unwrap())
            .unwrap() += 1;
    }
    for r in &candidate_rows {
        *candidate_outcomes
            .get_mut(r["outcome"].as_str().unwrap())
            .unwrap() += 1;
    }
    Ok(
        json!({"schema":SCHEMA,"status":"compared","outcome":outcome,"reasons":reasons,
        "source_sha256":edits::digest(&source.text),"candidate_sha256":edits::digest(&candidate.text),
        "source_parser_identity":source.parser_identity,"candidate_parser_identity":candidate.parser_identity,
        "parser_identity_equal":source.parser_identity==candidate.parser_identity,
        "counts":{"source_sentences":source.sentences.len(),"candidate_sentences":candidate.sentences.len(),
            "mapped_token_occurrences":map.len(),"source_licensed_omissions":source_omissions.len(),"candidate_licensed_insertions":candidate_insertions.len(),
            "source_unlicensed_unmapped_tokens":s.unlicensed.iter().map(Vec::len).sum::<usize>(),"candidate_unlicensed_unmapped_tokens":c.unlicensed.iter().map(Vec::len).sum::<usize>(),
            "split_source_sentences":s.links.iter().filter(|links|links.len()>1).count(),"merged_candidate_sentences":c.links.iter().filter(|links|links.len()>1).count(),
            "source_sentences_without_mapped_tokens":s.pairs.iter().filter(|p|p.is_empty()).count(),"candidate_sentences_without_mapped_tokens":c.pairs.iter().filter(|p|p.is_empty()).count(),
            "source_row_outcomes":source_outcomes,"candidate_row_outcomes":candidate_outcomes,
            "preserved_source_rows_with_index_shift":source_rows.iter().filter(|r|r["outcome"]=="observed_preserved" && r["positional_index_shift"]==true).count()},
        "source_sentences":source_rows,"candidate_sentences":candidate_rows,
        "license_validation":{"bounds_and_mapping_disjointness_checked":true,"carrier_rewrite_authorization":"caller_supplied; not established by sentence membership", "cross_document_numeric_index_overlap":"allowed; indices belong to different documents"},
        "interpretation":{"numeric_sentence_index_is_identity":false,"empty_sentence_equality_by_vacuity":false,"other_annotation_fields_compared":false,"caller_supplies_occurrence_correspondence":true},
        "semantic_equivalence_certified":false,"automatic_edit_license":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    fn doc(text: &str, words: &[(&str, &str, usize, usize)]) -> Document {
        let mut offset = 0;
        let tokens: Vec<_> = words
            .iter()
            .enumerate()
            .map(|(i, (word, pos, head, sentence))| {
                let start_byte = offset + text[offset..].find(word).unwrap();
                let end_byte = start_byte + word.len();
                offset = end_byte;
                Token {
                    i,
                    start_byte,
                    end_byte,
                    text: (*word).into(),
                    lemma: word.to_lowercase(),
                    pos: (*pos).into(),
                    tag: (*pos).into(),
                    dep: if i == *head { "ROOT" } else { "dep" }.into(),
                    head: *head,
                    sentence: *sentence,
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect();
        let sentences = if tokens.is_empty() {
            vec![]
        } else {
            (0..=tokens.last().unwrap().sentence)
                .map(|j| {
                    let a = tokens.iter().position(|t| t.sentence == j).unwrap();
                    let b = tokens.iter().rposition(|t| t.sentence == j).unwrap() + 1;
                    Sentence {
                        start_byte: tokens[a].start_byte,
                        end_byte: tokens[b - 1].end_byte,
                        root: tokens[a..b].iter().find(|t| t.i == t.head).unwrap().i,
                        token_start: a,
                        token_end: b,
                    }
                })
                .collect()
        };
        let d = Document {
            text: text.into(),
            parser_identity: "synthetic-membership-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&d).unwrap();
        d
    }
    fn reorder() -> (Document, Document, BTreeMap<usize, usize>) {
        let s = doc(
            "Éva gives the note to Éva.",
            &[
                ("Éva", "PROPN", 1, 0),
                ("gives", "VERB", 1, 0),
                ("the", "DET", 3, 0),
                ("note", "NOUN", 1, 0),
                ("to", "ADP", 1, 0),
                ("Éva", "PROPN", 4, 0),
                (".", "PUNCT", 1, 0),
            ],
        );
        let c = doc(
            "Éva gives Éva the note.",
            &[
                ("Éva", "PROPN", 1, 0),
                ("gives", "VERB", 1, 0),
                ("Éva", "PROPN", 1, 0),
                ("the", "DET", 4, 0),
                ("note", "NOUN", 1, 0),
                (".", "PUNCT", 1, 0),
            ],
        );
        (
            s,
            c,
            BTreeMap::from([(0, 0), (1, 1), (2, 3), (3, 4), (5, 2), (6, 5)]),
        )
    }
    fn split() -> (Document, Document, BTreeMap<usize, usize>) {
        let s = doc(
            "A runs. B runs. C runs.",
            &[
                ("A", "PROPN", 1, 0),
                ("runs", "VERB", 1, 0),
                (".", "PUNCT", 1, 0),
                ("B", "PROPN", 4, 0),
                ("runs", "VERB", 1, 0),
                (".", "PUNCT", 4, 0),
                ("C", "PROPN", 7, 1),
                ("runs", "VERB", 7, 1),
                (".", "PUNCT", 7, 1),
            ],
        );
        let c = doc(
            "A runs. B runs. C runs.",
            &[
                ("A", "PROPN", 1, 0),
                ("runs", "VERB", 1, 0),
                (".", "PUNCT", 1, 0),
                ("B", "PROPN", 4, 1),
                ("runs", "VERB", 4, 1),
                (".", "PUNCT", 4, 1),
                ("C", "PROPN", 7, 2),
                ("runs", "VERB", 7, 2),
                (".", "PUNCT", 7, 2),
            ],
        );
        (s, c, (0..9).map(|i| (i, i)).collect())
    }

    #[test]
    fn reordered_utf8_repeated_words_preserve_membership_with_licensed_carrier() {
        let (s, c, map) = reorder();
        let r = compare(&s, &c, &map, &BTreeSet::from([4]), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "observed_preserved");
        assert_eq!(r["counts"]["mapped_token_occurrences"], 6);
        let pairs = r["source_sentences"][0]["mapped_token_pairs"]
            .as_array()
            .unwrap();
        assert_eq!(pairs.iter().filter(|p| p["text"] == "Éva").count(), 2);
        assert_eq!(
            pairs.iter().find(|p| p["source_index"] == 5).unwrap()["candidate_index"],
            2
        );
        let reverse = map.into_iter().map(|(s, c)| (c, s)).collect();
        assert_eq!(
            compare(&c, &s, &reverse, &BTreeSet::new(), &BTreeSet::from([4])).unwrap()["outcome"],
            "observed_preserved"
        );
    }

    #[test]
    fn earlier_split_changes_global_partition_but_later_index_shift_is_preserved() {
        let (s, c, map) = split();
        let r = compare(&s, &c, &map, &BTreeSet::new(), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "changed");
        assert_eq!(r["counts"]["split_source_sentences"], 1);
        assert_eq!(r["source_sentences"][0]["outcome"], "changed");
        assert_eq!(r["source_sentences"][1]["outcome"], "observed_preserved");
        assert_eq!(
            r["source_sentences"][1]["matched_sentence_indices"],
            json!([2])
        );
        assert_eq!(r["source_sentences"][1]["positional_index_shift"], true);
        assert_eq!(r["candidate_sentences"][2]["outcome"], "observed_preserved");
    }

    #[test]
    fn merge_is_detected_in_both_directional_rows() {
        let (s, c, map) = split();
        let r = compare(&c, &s, &map, &BTreeSet::new(), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "changed");
        assert_eq!(r["counts"]["merged_candidate_sentences"], 1);
        assert_eq!(r["source_sentences"][0]["candidate_merge"], true);
        assert_eq!(r["source_sentences"][1]["candidate_merge"], true);
        assert_eq!(
            r["candidate_sentences"][0]["matched_sentence_indices"],
            json!([0, 1])
        );
    }

    #[test]
    fn unlicensed_unmapped_tokens_make_one_to_one_relation_unresolved() {
        let (s, c, mut map) = reorder();
        map.remove(&2);
        let r = compare(&s, &c, &map, &BTreeSet::from([4]), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "unresolved");
        assert_eq!(
            r["source_sentences"][0]["mapped_membership_one_to_one"],
            true
        );
        assert_eq!(r["counts"]["source_unlicensed_unmapped_tokens"], 1);
        assert_eq!(r["counts"]["candidate_unlicensed_unmapped_tokens"], 1);
        assert_eq!(
            r["source_sentences"][0]["other_side_unlicensed_unmapped_tokens"][0]["token"]["text"],
            "the"
        );
    }

    #[test]
    fn a_licensed_carrier_cannot_erase_or_create_a_sentence() {
        let s = doc(
            "A runs. to",
            &[
                ("A", "PROPN", 1, 0),
                ("runs", "VERB", 1, 0),
                (".", "PUNCT", 1, 0),
                ("to", "ADP", 3, 1),
            ],
        );
        let c = doc(
            "A runs.",
            &[
                ("A", "PROPN", 1, 0),
                ("runs", "VERB", 1, 0),
                (".", "PUNCT", 1, 0),
            ],
        );
        let map = (0..3).map(|i| (i, i)).collect();
        let r = compare(&s, &c, &map, &BTreeSet::from([3]), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "unresolved");
        assert_eq!(r["source_sentences"][0]["outcome"], "observed_preserved");
        assert_eq!(r["source_sentences"][1]["outcome"], "unresolved");
        assert_eq!(r["counts"]["source_unlicensed_unmapped_tokens"], 0);
        assert_eq!(
            compare(&c, &s, &map, &BTreeSet::new(), &BTreeSet::from([3])).unwrap()["outcome"],
            "unresolved"
        );
    }

    #[test]
    fn empty_documents_and_fully_unmapped_sentences_are_not_equal_by_vacuity() {
        let empty = doc("  ", &[]);
        assert_eq!(
            compare(
                &empty,
                &empty,
                &BTreeMap::new(),
                &BTreeSet::new(),
                &BTreeSet::new()
            )
            .unwrap()["outcome"],
            "unresolved"
        );
        let (s, c, _) = reorder();
        let r = compare(&s, &c, &BTreeMap::new(), &BTreeSet::new(), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "unresolved");
        assert_eq!(
            r["source_sentences"][0]["matched_sentence_indices"],
            json!([])
        );
    }

    #[test]
    fn malformed_mapping_and_overlapping_or_out_of_bounds_licenses_are_errors() {
        let (s, c, map) = reorder();
        for bad in [
            BTreeMap::from([(99, 0)]),
            BTreeMap::from([(0, 99)]),
            BTreeMap::from([(0, 1)]),
            BTreeMap::from([(0, 0), (5, 0)]),
        ] {
            assert!(compare(&s, &c, &bad, &BTreeSet::new(), &BTreeSet::new()).is_err());
        }
        assert!(compare(&s, &c, &map, &BTreeSet::from([0]), &BTreeSet::new()).is_err());
        assert!(compare(&s, &c, &map, &BTreeSet::new(), &BTreeSet::from([0])).is_err());
        assert!(compare(&s, &c, &map, &BTreeSet::from([99]), &BTreeSet::new()).is_err());
        assert!(compare(&s, &c, &map, &BTreeSet::new(), &BTreeSet::from([99])).is_err());
        let mut invalid = s.clone();
        invalid.tokens[0].head = 99;
        assert!(compare(&invalid, &c, &map, &BTreeSet::new(), &BTreeSet::new()).is_err());
    }

    #[test]
    fn known_split_takes_precedence_over_unmapped_uncertainty() {
        let (s, c, mut map) = split();
        map.remove(&8);
        let r = compare(&s, &c, &map, &BTreeSet::new(), &BTreeSet::new()).unwrap();
        assert_eq!(r["outcome"], "changed");
        assert_eq!(r["source_sentences"][1]["outcome"], "unresolved");
        assert_eq!(r["source_sentences"][0]["outcome"], "changed");
    }
}

//! Terminal mark roles from exact character occurrences and supplied sentences.
//! This records boundary evidence without deciding whether a period abbreviates.
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits,
    syntax::{self, Document},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const SCHEMA: &str = "slopninja-orthographic-evidence-v1";
const CLOSERS: &str = "\"'’”»›)]}";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    TerminalMark,
    FollowingCloser,
}

#[derive(Clone, Debug, Serialize)]
struct Character {
    span: [usize; 2],
    character: char,
    role: Role,
    token_index: usize,
    token_relative_span: [usize; 2],
}

#[derive(Clone, Debug, Serialize)]
struct Sequence {
    sentence_index: usize,
    sentence_span: [usize; 2],
    characters: Vec<Character>,
}

fn terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}

fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{001c}'..='\u{001f}').contains(&c)
}

fn sequences(doc: &Document) -> Result<Vec<Sequence>> {
    let mut result = Vec::new();
    for (sentence_index, sentence) in doc.sentences.iter().enumerate() {
        let slice = &doc.text[sentence.start_byte..sentence.end_byte];
        // Read only the maximal suffix made of terminal marks, explicit
        // closers and whitespace. Any leading closer before its first mark is
        // outside the protected sequence. Interleaved mark/closer runs remain
        // visible, including ellipses and punctuation inside nested closers.
        let mut suffix = Vec::new();
        for (offset, character) in slice.char_indices().rev() {
            if terminal(character) || CLOSERS.contains(character) || whitespace(character) {
                suffix.push((sentence.start_byte + offset, character));
            } else {
                break;
            }
        }
        suffix.reverse();
        let Some(first_mark) = suffix.iter().position(|(_, c)| terminal(*c)) else {
            continue;
        };
        let mut characters = Vec::new();
        for &(start, character) in &suffix[first_mark..] {
            if whitespace(character) {
                continue;
            }
            let end = start + character.len_utf8();
            let token = doc.tokens[sentence.token_start..sentence.token_end]
                .iter()
                .find(|t| t.start_byte <= start && end <= t.end_byte)
                .context("Terminal suffix character lacks a token occurrence")?;
            characters.push(Character {
                span: [start, end],
                character,
                role: if terminal(character) {
                    Role::TerminalMark
                } else {
                    Role::FollowingCloser
                },
                token_index: token.i,
                token_relative_span: [start - token.start_byte, end - token.start_byte],
            });
        }
        result.push(Sequence {
            sentence_index,
            sentence_span: [sentence.start_byte, sentence.end_byte],
            characters,
        });
    }
    Ok(result)
}

fn finding(
    findings: &mut Vec<Value>,
    direction: &str,
    condition: &str,
    outcome: &str,
    evidence: Value,
) {
    findings.push(
        json!({"direction":direction,"condition":condition,"outcome":outcome,"evidence":evidence}),
    );
}

#[allow(clippy::too_many_arguments)]
fn check_direction(
    reference: &Document,
    observed: &Document,
    token_map: &BTreeMap<usize, usize>,
    reference_sequences: &[Sequence],
    observed_sequences: &[Sequence],
    direction: &str,
    findings: &mut Vec<Value>,
    alignments: &mut Vec<Value>,
) -> Result<()> {
    let observed_characters: BTreeMap<_, _> = observed_sequences
        .iter()
        .enumerate()
        .flat_map(|(sequence_index, sequence)| {
            sequence
                .characters
                .iter()
                .map(move |c| (c.span, (sequence_index, c)))
        })
        .collect();
    for (sequence_index, sequence) in reference_sequences.iter().enumerate() {
        let mut mapped = Vec::new();
        let mut destinations = BTreeSet::new();
        let mut all_roles_known_equal = true;
        for character in &sequence.characters {
            let token = &reference.tokens[character.token_index];
            let Some(&target_index) = token_map.get(&character.token_index) else {
                all_roles_known_equal = false;
                finding(
                    findings,
                    direction,
                    "terminal_character_unmapped",
                    "unresolved",
                    json!({"reference_sequence":sequence_index,"reference_character":character,"reference_token":token}),
                );
                continue;
            };
            let target = &observed.tokens[target_index];
            if token.text != target.text {
                all_roles_known_equal = false;
                finding(
                    findings,
                    direction,
                    "mapped_token_text_differs",
                    "unresolved",
                    json!({"reference_character":character,"reference_token":token,"observed_token":target,
                        "reason":"Character identity is not inferred inside a changed token"}),
                );
                continue;
            }
            let target_span = [
                target.start_byte + character.token_relative_span[0],
                target.start_byte + character.token_relative_span[1],
            ];
            ensure!(
                reference.text.get(character.span[0]..character.span[1])
                    == observed.text.get(target_span[0]..target_span[1]),
                "Identical-token character correspondence differs"
            );
            mapped.push(target_span);
            let target_role = observed_characters.get(&target_span);
            alignments.push(json!({"direction":direction,
                "reference_character":character,"reference_token":token,
                "observed_span":target_span,"observed_token":target,
                "observed_terminal_role":target_role.map(|(_, c)|c.role),
                "observed_sequence":target_role.map(|(i, _)|i)}));
            match target_role {
                Some((target_sequence, target_character))
                    if target_character.role == character.role =>
                {
                    destinations.insert(*target_sequence);
                }
                _ => {
                    all_roles_known_equal = false;
                    finding(
                        findings,
                        direction,
                        "terminal_suffix_role_changed",
                        "changed",
                        json!({"reference_character":character,"observed_span":target_span,
                            "observed_terminal_role":target_role.map(|(_, c)|c.role),
                            "observed_token":target,
                            "null_role_means":"Outside the observed terminal suffix, without an abbreviation judgment"}),
                    );
                }
            }
        }
        if !all_roles_known_equal {
            continue;
        }
        if destinations.len() != 1 {
            finding(
                findings,
                direction,
                "terminal_sequence_split",
                "changed",
                json!({"reference_sequence":sequence_index,"observed_sequences":destinations,"mapped_character_spans":mapped}),
            );
            continue;
        }
        let target_sequence = *destinations.iter().next().unwrap();
        let expected: Vec<_> = observed_sequences[target_sequence]
            .characters
            .iter()
            .map(|c| c.span)
            .collect();
        if mapped == expected {
            finding(
                findings,
                direction,
                "terminal_sequence_occurrences_preserved",
                "observed_preserved",
                json!({"reference_sequence":sequence_index,"observed_sequence":target_sequence,"mapped_character_spans":mapped}),
            );
        } else if mapped.len() == expected.len() {
            finding(
                findings,
                direction,
                "terminal_sequence_order_changed",
                "changed",
                json!({"reference_sequence":sequence_index,"observed_sequence":target_sequence,
                    "mapped_character_spans":mapped,"observed_character_spans":expected}),
            );
        } else {
            // An extra terminal character may have no token correspondence.
            // The opposite direction records whether it is unmapped or moves
            // from an observed interior position. Do not invent its identity.
            finding(
                findings,
                direction,
                "terminal_sequence_correspondence_incomplete",
                "unresolved",
                json!({"reference_sequence":sequence_index,"observed_sequence":target_sequence,
                    "mapped_character_spans":mapped,"observed_character_spans":expected}),
            );
        }
    }
    Ok(())
}

/// Compare terminal character occurrences in both directions under an existing
/// injective token map. Documents and exact token identity supply all evidence.
pub fn compare(
    source: &Document,
    candidate: &Document,
    map: &BTreeMap<usize, usize>,
) -> Result<Value> {
    syntax::validate(source).context("Invalid orthographic source annotation")?;
    syntax::validate(candidate).context("Invalid orthographic candidate annotation")?;
    ensure!(
        source.parser_identity == candidate.parser_identity,
        "Parser identities differ"
    );
    let mut reverse = BTreeMap::new();
    for (&a, &b) in map {
        ensure!(
            a < source.tokens.len() && b < candidate.tokens.len(),
            "Token map index out of range"
        );
        ensure!(reverse.insert(b, a).is_none(), "Token map is not injective");
    }
    let source_sequences = sequences(source)?;
    let candidate_sequences = sequences(candidate)?;
    let mut findings = Vec::new();
    let mut alignments = Vec::new();
    check_direction(
        source,
        candidate,
        map,
        &source_sequences,
        &candidate_sequences,
        "source_to_candidate",
        &mut findings,
        &mut alignments,
    )?;
    check_direction(
        candidate,
        source,
        &reverse,
        &candidate_sequences,
        &source_sequences,
        "candidate_to_source",
        &mut findings,
        &mut alignments,
    )?;
    let absent = source_sequences.is_empty() && candidate_sequences.is_empty();
    let outcome = if findings.iter().any(|f| f["outcome"] == "changed") {
        "changed"
    } else if findings.iter().any(|f| f["outcome"] == "unresolved") {
        "unresolved"
    } else {
        "observed_preserved"
    };
    Ok(json!({
        "schema":SCHEMA,"status":if absent{"observed_absent"}else{"compared"},"outcome":outcome,
        "source_sha256":edits::digest(&source.text),"candidate_sha256":edits::digest(&candidate.text),
        "parser_identity":source.parser_identity,
        "source_terminal_sequences":source_sequences,"candidate_terminal_sequences":candidate_sequences,
        "character_alignment":alignments,"findings":findings,
        "counts":{"mapped_token_pairs":map.len(),"source_terminal_sequences":source_sequences.len(),
            "candidate_terminal_sequences":candidate_sequences.len(),
            "source_terminal_characters":source_sequences.iter().flat_map(|s|&s.characters).filter(|c|c.role==Role::TerminalMark).count(),
            "candidate_terminal_characters":candidate_sequences.iter().flat_map(|s|&s.characters).filter(|c|c.role==Role::TerminalMark).count(),
            "changed_findings":findings.iter().filter(|f|f["outcome"]=="changed").count(),
            "unresolved_findings":findings.iter().filter(|f|f["outcome"]=="unresolved").count()},
        "policy":{"terminal_marks":".!?…","closing_characters":CLOSERS,
            "whitespace_in_suffix":"Ignored for mark/closer sequence equality; exact non-whitespace character spans retained",
            "boundary_source":"Only the supplied validated parsed sentence spans; no inferred sentence repair",
            "alignment":"Exact UTF-8 character offset inside an identical mapped token; surface-search alignment forbidden"},
        "ambiguity_notes":["A period in a lexical token is retained as both token evidence and a character occurrence. No abbreviation or grammaticality decision is inferred.",
            "Sentence-final abbreviation periods can have the observed terminal role; interior abbreviation periods have no such role unless supplied sentence boundaries put them at the end.",
            "Observed absence is absence of eligible suffix evidence, not a semantic or orthographic correctness certificate.",
            "Whitespace and punctuation outside the declared terminal/closer suffix are outside this check; other preservation checks remain required."],
        "automatic_edit_license":false,"semantic_equivalence_certified":false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::{Sentence, Token};

    fn document(text: &str, items: &[(&str, &str)]) -> Document {
        let mut cursor = 0;
        let tokens: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, (word, pos))| {
                let start_byte = cursor + text[cursor..].find(word).unwrap();
                let end_byte = start_byte + word.len();
                cursor = end_byte;
                Token {
                    i,
                    start_byte,
                    end_byte,
                    text: (*word).into(),
                    lemma: word.to_lowercase(),
                    pos: (*pos).into(),
                    tag: (*pos).into(),
                    dep: if i == 0 { "ROOT" } else { "dep" }.into(),
                    head: 0,
                    sentence: 0,
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect();
        let sentences = if tokens.is_empty() {
            vec![]
        } else {
            vec![Sentence {
                start_byte: tokens[0].start_byte,
                end_byte: tokens.last().unwrap().end_byte,
                token_start: 0,
                token_end: tokens.len(),
                root: 0,
            }]
        };
        let d = Document {
            text: text.into(),
            parser_identity: "synthetic-orthographic-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&d).unwrap();
        d
    }

    fn identity(doc: &Document) -> BTreeMap<usize, usize> {
        (0..doc.tokens.len()).map(|i| (i, i)).collect()
    }

    fn fused_pair() -> (Document, Document, BTreeMap<usize, usize>) {
        let source = document(
            "The registrar gives the form to I.",
            &[
                ("The", "DET"),
                ("registrar", "NOUN"),
                ("gives", "VERB"),
                ("the", "DET"),
                ("form", "NOUN"),
                ("to", "ADP"),
                ("I.", "PROPN"),
            ],
        );
        let candidate = document(
            "The registrar gives I. the form",
            &[
                ("The", "DET"),
                ("registrar", "NOUN"),
                ("gives", "VERB"),
                ("I.", "PROPN"),
                ("the", "DET"),
                ("form", "NOUN"),
            ],
        );
        (
            source,
            candidate,
            BTreeMap::from([(0, 0), (1, 1), (2, 2), (3, 4), (4, 5), (6, 3)]),
        )
    }

    #[test]
    fn moved_fused_final_period_changes_role_in_both_directions() {
        let (source, candidate, map) = fused_pair();
        for (a, b, m) in [
            (&source, &candidate, map.clone()),
            (
                &candidate,
                &source,
                map.iter().map(|(&a, &b)| (b, a)).collect(),
            ),
        ] {
            let r = compare(a, b, &m).unwrap();
            assert_eq!(r["outcome"], "changed");
            assert!(
                r["findings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["condition"] == "terminal_suffix_role_changed")
            );
            assert_eq!(r["automatic_edit_license"], false);
        }
        let r = compare(&source, &candidate, &map).unwrap();
        assert_eq!(
            r["source_terminal_sequences"][0]["characters"][0]["token_index"],
            6
        );
        assert_eq!(
            r["character_alignment"][0]["reference_token"]["pos"],
            "PROPN"
        );
        assert_eq!(
            r["character_alignment"][0]["reference_token"]["is_punct"],
            false
        );
    }

    #[test]
    fn internal_abbreviation_can_move_while_terminal_period_stays_terminal() {
        let a = document(
            "Ava lends Dr. Grey a book.",
            &[
                ("Ava", "PROPN"),
                ("lends", "VERB"),
                ("Dr.", "PROPN"),
                ("Grey", "PROPN"),
                ("a", "DET"),
                ("book", "NOUN"),
                (".", "PUNCT"),
            ],
        );
        let b = document(
            "Ava lends a book to Dr. Grey.",
            &[
                ("Ava", "PROPN"),
                ("lends", "VERB"),
                ("a", "DET"),
                ("book", "NOUN"),
                ("to", "ADP"),
                ("Dr.", "PROPN"),
                ("Grey", "PROPN"),
                (".", "PUNCT"),
            ],
        );
        let map = BTreeMap::from([(0, 0), (1, 1), (2, 5), (3, 6), (4, 2), (5, 3), (6, 7)]);
        let r = compare(&a, &b, &map).unwrap();
        assert_eq!(r["outcome"], "observed_preserved");
        assert_eq!(r["counts"]["source_terminal_characters"], 1);
        assert_eq!(r["counts"]["candidate_terminal_characters"], 1);
    }

    #[test]
    fn unicode_ellipsis_and_closers_retain_exact_character_spans() {
        let a = document(
            "Zoë says (\"yes…\").",
            &[
                ("Zoë", "PROPN"),
                ("says", "VERB"),
                ("(", "PUNCT"),
                ("\"", "PUNCT"),
                ("yes", "INTJ"),
                ("…", "PUNCT"),
                ("\"", "PUNCT"),
                (")", "PUNCT"),
                (".", "PUNCT"),
            ],
        );
        let r = compare(&a, &a, &identity(&a)).unwrap();
        assert_eq!(r["outcome"], "observed_preserved");
        assert_eq!(r["counts"]["source_terminal_characters"], 2);
        let chars = r["source_terminal_sequences"][0]["characters"]
            .as_array()
            .unwrap();
        assert_eq!(chars.len(), 4);
        assert_eq!(chars[0]["character"], "…");
        assert_eq!(
            chars[0]["span"][1].as_u64().unwrap() - chars[0]["span"][0].as_u64().unwrap(),
            3
        );
        let dots = document("Wait...", &[("Wait", "VERB"), ("...", "PUNCT")]);
        let r = compare(&dots, &dots, &identity(&dots)).unwrap();
        assert_eq!(r["counts"]["source_terminal_characters"], 3);
        assert_eq!(r["outcome"], "observed_preserved");
    }

    #[test]
    fn repeated_identical_tokens_are_never_aligned_by_surface_search() {
        let a = document(
            "I. lends to I.",
            &[
                ("I.", "PROPN"),
                ("lends", "VERB"),
                ("to", "ADP"),
                ("I.", "PROPN"),
            ],
        );
        let r = compare(&a, &a, &BTreeMap::from([(0, 3), (1, 1), (2, 2), (3, 0)])).unwrap();
        assert_eq!(r["outcome"], "changed");
        assert_eq!(r["counts"]["changed_findings"], 2);
    }

    #[test]
    fn unmapped_suffix_and_changed_token_text_remain_unresolved() {
        let a = document(
            "It ends.",
            &[("It", "PRON"), ("ends", "VERB"), (".", "PUNCT")],
        );
        let r = compare(&a, &a, &BTreeMap::from([(0, 0), (1, 1)])).unwrap();
        assert_eq!(r["outcome"], "unresolved");
        let b = document(
            "It ends!",
            &[("It", "PRON"), ("ends", "VERB"), ("!", "PUNCT")],
        );
        let r = compare(&a, &b, &identity(&a)).unwrap();
        assert_eq!(r["outcome"], "unresolved");
        assert!(
            r["findings"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f["condition"] == "mapped_token_text_differs")
        );
    }

    #[test]
    fn closing_sequence_order_changes_are_observed() {
        let a = document(
            "It ends.)]",
            &[
                ("It", "PRON"),
                ("ends", "VERB"),
                (".", "PUNCT"),
                (")", "PUNCT"),
                ("]", "PUNCT"),
            ],
        );
        let b = document(
            "It ends.])",
            &[
                ("It", "PRON"),
                ("ends", "VERB"),
                (".", "PUNCT"),
                ("]", "PUNCT"),
                (")", "PUNCT"),
            ],
        );
        let r = compare(
            &a,
            &b,
            &BTreeMap::from([(0, 0), (1, 1), (2, 2), (3, 4), (4, 3)]),
        )
        .unwrap();
        assert_eq!(r["outcome"], "changed");
        assert!(
            r["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["condition"] == "terminal_sequence_order_changed")
        );
    }

    #[test]
    fn supplied_sentence_boundaries_are_used_without_repair() {
        let (source, mut candidate, map) = fused_pair();
        // The exact bad candidate bytes now have a supplied boundary after I.
        // This module records that boundary evidence and cannot declare it false.
        candidate.sentences = vec![
            Sentence {
                start_byte: 0,
                end_byte: candidate.tokens[3].end_byte,
                root: 0,
                token_start: 0,
                token_end: 4,
            },
            Sentence {
                start_byte: candidate.tokens[4].start_byte,
                end_byte: candidate.text.len(),
                root: 4,
                token_start: 4,
                token_end: 6,
            },
        ];
        candidate.tokens[4].head = 4;
        candidate.tokens[4].dep = "ROOT".into();
        candidate.tokens[4].sentence = 1;
        candidate.tokens[5].head = 4;
        candidate.tokens[5].sentence = 1;
        let r = compare(&source, &candidate, &map).unwrap();
        assert_eq!(r["outcome"], "observed_preserved");
        assert_eq!(r["counts"]["candidate_terminal_sequences"], 1);
    }

    #[test]
    fn absence_errors_and_known_change_precedence_are_explicit() {
        let a = document("Plain words", &[("Plain", "ADJ"), ("words", "NOUN")]);
        let r = compare(&a, &a, &identity(&a)).unwrap();
        assert_eq!(r["status"], "observed_absent");
        assert_eq!(r["outcome"], "observed_preserved");
        assert!(compare(&a, &a, &BTreeMap::from([(0, 0), (1, 0)])).is_err());
        let (source, candidate, mut map) = fused_pair();
        map.remove(&6);
        assert_eq!(
            compare(&source, &candidate, &map).unwrap()["outcome"],
            "unresolved"
        );
        let (mut source, mut candidate, map) = fused_pair();
        for doc in [&mut source, &mut candidate] {
            let start_byte = doc.text.len();
            doc.text.push('!');
            doc.tokens.push(Token {
                i: doc.tokens.len(),
                start_byte,
                end_byte: doc.text.len(),
                text: "!".into(),
                lemma: "!".into(),
                pos: "PUNCT".into(),
                tag: "PUNCT".into(),
                dep: "dep".into(),
                head: 0,
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: true,
                is_space: false,
            });
            doc.sentences[0].end_byte = doc.text.len();
            doc.sentences[0].token_end = doc.tokens.len();
        }
        // The moved I. period has a known role change; the two added bang
        // tokens are deliberately absent from the supplied map.
        let r = compare(&source, &candidate, &map).unwrap();
        assert_eq!(r["outcome"], "changed");
        assert!(r["counts"]["unresolved_findings"].as_u64().unwrap() > 0);
    }
}

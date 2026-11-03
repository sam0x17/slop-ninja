//! Exact source-sentence slices and closed projections of saved annotations.
//! Candidate segmentation is recorded without changing the source-defined slice.

use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Candidate, Edit},
    syntax::{self, Document, Sentence},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-sentence-context-projection-v1";

#[derive(Deserialize)]
struct Role {
    head_index: usize,
    span: [usize; 2],
    token_indices: Vec<usize>,
}

#[derive(Deserialize)]
struct Segment {
    source: [usize; 2],
    candidate: [usize; 2],
}

#[derive(Deserialize)]
struct Input {
    predicate_index: usize,
    roles: BTreeMap<String, Role>,
    candidate: Candidate,
    inverse_edits: Vec<Edit>,
    byte_segments: Vec<Segment>,
}

fn slice(text: &str, span: [usize; 2]) -> Result<&str> {
    ensure!(span[0] <= span[1], "Reversed byte span");
    text.get(span[0]..span[1])
        .context("Byte span is outside text or splits UTF-8")
}

fn contains(outer: [usize; 2], inner: [usize; 2]) -> bool {
    outer[0] <= inner[0] && inner[1] <= outer[1]
}

fn unavailable(reason: &str, evidence: Value) -> Value {
    json!({"schema":SCHEMA,"status":"unavailable","reason":reason,"evidence":evidence,
        "semantic_equivalence_certified":false,"automatic_edit_license":false})
}

/// Return a rebased Document only when `span` is exactly one complete sentence
/// in the supplied full annotation. No token, edge, gap or whitespace is repaired.
pub fn project(doc: &Document, span: [usize; 2]) -> Result<Value> {
    syntax::validate(doc).context("Invalid full annotation for projection")?;
    let text = slice(&doc.text, span)?;
    let Some((sentence_index, sentence)) = doc
        .sentences
        .iter()
        .enumerate()
        .find(|(_, s)| [s.start_byte, s.end_byte] == span)
    else {
        let overlapping: Vec<_> = doc
            .sentences
            .iter()
            .enumerate()
            .filter(|(_, s)| s.start_byte < span[1] && span[0] < s.end_byte)
            .map(|(i, s)| json!({"index":i,"sentence":s}))
            .collect();
        return Ok(unavailable(
            "span_is_not_one_exact_full_sentence",
            json!({"requested_span":span,"overlapping_sentences":overlapping}),
        ));
    };
    let original_tokens = &doc.tokens[sentence.token_start..sentence.token_end];
    if original_tokens
        .iter()
        .any(|t| !(sentence.token_start..sentence.token_end).contains(&t.head))
    {
        return Ok(unavailable(
            "sentence_dependency_not_closed",
            json!({"requested_span":span,"sentence":sentence}),
        ));
    }
    let tokens = original_tokens
        .iter()
        .map(|t| {
            let mut token = t.clone();
            token.i -= sentence.token_start;
            token.head -= sentence.token_start;
            token.sentence = 0;
            token.start_byte -= span[0];
            token.end_byte -= span[0];
            token
        })
        .collect::<Vec<_>>();
    let projected = Document {
        text: text.into(),
        parser_identity: doc.parser_identity.clone(),
        sentences: vec![Sentence {
            start_byte: 0,
            end_byte: text.len(),
            root: sentence.root - sentence.token_start,
            token_start: 0,
            token_end: tokens.len(),
        }],
        tokens,
    };
    syntax::validate(&projected).context("Rebased closed sentence failed validation")?;
    Ok(
        json!({"schema":SCHEMA,"status":"available","full_source_sha256":edits::digest(&doc.text),
        "span":span,"source_sentence_index":sentence_index,"source_token_start":sentence.token_start,
        "document":projected,"semantic_equivalence_certified":false,"automatic_edit_license":false}),
    )
}

fn rebase(edits: &[Edit], offset: usize) -> Result<Vec<Edit>> {
    edits
        .iter()
        .map(|edit| {
            Ok(Edit {
                start_byte: edit
                    .start_byte
                    .checked_sub(offset)
                    .context("Edit starts before projected span")?,
                end_byte: edit
                    .end_byte
                    .checked_sub(offset)
                    .context("Edit ends before projected span")?,
                expected: edit.expected.clone(),
                replacement: edit.replacement.clone(),
            })
        })
        .collect()
}

fn disjoint(mut spans: Vec<[usize; 2]>) -> bool {
    spans.retain(|s| s[0] != s[1]);
    spans.sort();
    spans.windows(2).all(|pair| pair[0][1] <= pair[1][0])
}

fn validate_roles(source: &Document, input: &Input) -> Result<()> {
    ensure!(
        input.roles.keys().map(String::as_str).collect::<Vec<_>>()
            == ["agent", "recipient", "theme"],
        "Expected exactly Agent, Theme and Recipient role records"
    );
    let mut used = BTreeSet::new();
    for (name, role) in &input.roles {
        slice(&source.text, role.span)?;
        ensure!(
            !role.token_indices.is_empty() && role.token_indices.windows(2).all(|w| w[0] < w[1]),
            "Role {name} has missing, repeated or unordered token anchors"
        );
        ensure!(
            role.token_indices.contains(&role.head_index),
            "Role {name} omits its head anchor"
        );
        for &i in &role.token_indices {
            let token = source
                .tokens
                .get(i)
                .context("Role token index out of bounds")?;
            ensure!(
                contains(role.span, [token.start_byte, token.end_byte]),
                "Role span omits a token"
            );
            ensure!(
                i != input.predicate_index && used.insert(i),
                "Role token anchors overlap each other or the predicate"
            );
        }
        let first = &source.tokens[role.token_indices[0]];
        let last = &source.tokens[*role.token_indices.last().unwrap()];
        ensure!(
            role.span == [first.start_byte, last.end_byte],
            "Role span differs from token anchor bounds"
        );
    }
    Ok(())
}

fn validate_segments(source: &Document, candidate: &Document, input: &Input) -> Result<()> {
    ensure!(
        !input.byte_segments.is_empty(),
        "Missing byte correspondence segments"
    );
    for segment in &input.byte_segments {
        ensure!(
            slice(&source.text, segment.source)? == slice(&candidate.text, segment.candidate)?,
            "Byte correspondence segment text differs"
        );
    }
    ensure!(
        disjoint(input.byte_segments.iter().map(|s| s.source).collect())
            && disjoint(input.byte_segments.iter().map(|s| s.candidate).collect()),
        "Byte correspondence segments overlap"
    );
    Ok(())
}

/// Prepare source-defined slices without requiring the candidate parser to agree
/// on sentence boundaries. This validates bindings and offsets; it does not
/// replay the argument policy, certify semantics, or license the edit.
pub fn prepare(source: &Document, candidate: &Document, proposal: &Value) -> Result<Value> {
    syntax::validate(source).context("Invalid source annotation")?;
    syntax::validate(candidate).context("Invalid candidate annotation")?;
    ensure!(
        source.parser_identity == candidate.parser_identity,
        "Source and candidate parser identities differ"
    );
    let input: Input =
        serde_json::from_value(proposal.clone()).context("Malformed original proposal")?;
    ensure!(
        input.candidate.parser_identity == source.parser_identity,
        "Proposal parser identity differs"
    );
    ensure!(
        input.candidate.source_sha256 == edits::digest(&source.text)
            && input.candidate.candidate_sha256 == edits::digest(&candidate.text)
            && input.candidate.text == candidate.text,
        "Proposal source/candidate text binding differs"
    );
    ensure!(
        !input.candidate.edits.is_empty()
            && !input.inverse_edits.is_empty()
            && source.text != candidate.text,
        "Expected a changed candidate and nonempty forward/inverse patches"
    );
    ensure!(
        edits::apply(&source.text, &input.candidate.edits)? == candidate.text,
        "Full forward patches do not produce candidate"
    );
    ensure!(
        edits::apply(&candidate.text, &input.inverse_edits)? == source.text,
        "Full inverse patches do not restore source"
    );
    let predicate = source
        .tokens
        .get(input.predicate_index)
        .context("Source predicate index out of bounds")?;
    validate_roles(source, &input)?;
    validate_segments(source, candidate, &input)?;
    let sentence = &source.sentences[predicate.sentence];
    let source_span = [sentence.start_byte, sentence.end_byte];
    let predicate_span = [predicate.start_byte, predicate.end_byte];
    if input
        .candidate
        .edits
        .iter()
        .any(|e| !contains(source_span, [e.start_byte, e.end_byte]))
    {
        return Ok(unavailable(
            "forward_edit_outside_source_predicate_sentence",
            json!({"source_span":source_span,"predicate_span":predicate_span,"forward_edits":input.candidate.edits}),
        ));
    }
    if input.roles.values().any(|r| !contains(source_span, r.span)) {
        return Ok(unavailable(
            "role_outside_source_predicate_sentence",
            json!({"source_span":source_span,"roles":proposal["roles"]}),
        ));
    }
    if input.candidate.edits.iter().any(|e| {
        e.start_byte < predicate_span[1] && predicate_span[0] < e.end_byte
            || e.start_byte == e.end_byte
                && predicate_span[0] < e.start_byte
                && e.start_byte < predicate_span[1]
    }) {
        return Ok(unavailable(
            "predicate_intersects_forward_edit",
            json!({"predicate_span":predicate_span}),
        ));
    }
    let removed = input.candidate.edits.iter().try_fold(0usize, |total, e| {
        total
            .checked_add(e.end_byte - e.start_byte)
            .context("Removed byte length overflow")
    })?;
    let added = input.candidate.edits.iter().try_fold(0usize, |total, e| {
        total
            .checked_add(e.replacement.len())
            .context("Inserted byte length overflow")
    })?;
    let candidate_end = source_span[1]
        .checked_sub(removed)
        .and_then(|n| n.checked_add(added))
        .context("Projected candidate end overflow")?;
    let candidate_span = [source_span[0], candidate_end];
    let source_text = slice(&source.text, source_span)?;
    let candidate_text = slice(&candidate.text, candidate_span)?;
    ensure!(
        source.text[..source_span[0]] == candidate.text[..candidate_span[0]]
            && source.text[source_span[1]..] == candidate.text[candidate_span[1]..],
        "Projected slice does not preserve untouched prefix/suffix bytes"
    );
    if input
        .inverse_edits
        .iter()
        .any(|e| !contains(candidate_span, [e.start_byte, e.end_byte]))
    {
        return Ok(unavailable(
            "inverse_edit_outside_source_defined_candidate_span",
            json!({"candidate_span":candidate_span,"inverse_edits":input.inverse_edits}),
        ));
    }
    let forward_edits = rebase(&input.candidate.edits, source_span[0])?;
    let inverse_edits = rebase(&input.inverse_edits, candidate_span[0])?;
    ensure!(
        edits::apply(source_text, &forward_edits)? == candidate_text,
        "Rebased forward patches differ from exact candidate slice"
    );
    ensure!(
        edits::apply(candidate_text, &inverse_edits)? == source_text,
        "Rebased inverse patches differ from exact source slice"
    );
    let predicate_matches: Vec<_> = input
        .byte_segments
        .iter()
        .filter(|s| contains(s.source, predicate_span))
        .collect();
    ensure!(
        predicate_matches.len() == 1,
        "Predicate lacks unique byte-segment correspondence"
    );
    let segment = predicate_matches[0];
    let candidate_predicate = [
        segment.candidate[0] + predicate_span[0] - segment.source[0],
        segment.candidate[0] + predicate_span[1] - segment.source[0],
    ];
    ensure!(
        contains(candidate_span, candidate_predicate)
            && slice(&candidate.text, candidate_predicate)? == predicate.text,
        "Mapped predicate falls outside slice or changes bytes"
    );
    // An unchanged predicate must map to the position implied by the patches.
    // This rejects a segment redirected to another occurrence of the same word.
    let mut expected = predicate_span;
    for edit in &input.candidate.edits {
        if edit.end_byte <= predicate_span[0] {
            for offset in &mut expected {
                *offset = offset
                    .checked_sub(edit.end_byte - edit.start_byte)
                    .and_then(|n| n.checked_add(edit.replacement.len()))
                    .context("Predicate correspondence offset overflow")?;
            }
        }
    }
    ensure!(
        candidate_predicate == expected,
        "Predicate byte segment contradicts forward patch correspondence"
    );
    let source_projection = project(source, source_span)?;
    ensure!(
        source_projection["status"] == "available",
        "Source-defined sentence projection unavailable"
    );
    let candidate_projection = project(candidate, candidate_span)?;
    Ok(
        json!({"schema":SCHEMA,"status":"prepared","parser_identity":source.parser_identity,
        "source_full_sha256":edits::digest(&source.text),"candidate_full_sha256":edits::digest(&candidate.text),
        "source_span":source_span,"candidate_span":candidate_span,"source_text":source_text,"candidate_text":candidate_text,
        "source_predicate_span":[predicate_span[0]-source_span[0],predicate_span[1]-source_span[0]],
        "candidate_predicate_span":[candidate_predicate[0]-candidate_span[0],candidate_predicate[1]-candidate_span[0]],
        "length_delta_bytes":i64::try_from(added)?-i64::try_from(removed)?,
        "forward_edits":forward_edits,"inverse_edits":inverse_edits,
        "source_projection":source_projection,"candidate_projection":candidate_projection,
        "semantic_equivalence_certified":false,"automatic_edit_license":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::{edits::RuleIdentity, syntax::Token};

    fn doc(text: &str, words: &[(&str, &str, &str, usize, usize)]) -> Document {
        let mut offset = 0;
        let tokens: Vec<_> = words
            .iter()
            .enumerate()
            .map(|(i, (word, pos, dep, head, sentence))| {
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
                    dep: (*dep).into(),
                    head: *head,
                    sentence: *sentence,
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect();
        let sentences = (0..=tokens.last().unwrap().sentence)
            .map(|s| {
                let start = tokens.iter().position(|t| t.sentence == s).unwrap();
                let end = tokens.iter().rposition(|t| t.sentence == s).unwrap() + 1;
                Sentence {
                    start_byte: tokens[start].start_byte,
                    end_byte: tokens[end - 1].end_byte,
                    root: tokens[start..end].iter().find(|t| t.head == t.i).unwrap().i,
                    token_start: start,
                    token_end: end,
                }
            })
            .collect();
        let d = Document {
            text: text.into(),
            parser_identity: "synthetic-sentence-context-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&d).unwrap();
        d
    }

    fn pair() -> (Document, Document, Value) {
        let source = doc(
            "Éva rests.  Mira gives the parcel to the clerk.\nMira waits.",
            &[
                ("Éva", "PROPN", "nsubj", 1, 0),
                ("rests", "VERB", "ROOT", 1, 0),
                (".", "PUNCT", "punct", 1, 0),
                ("Mira", "PROPN", "nsubj", 4, 1),
                ("gives", "VERB", "ROOT", 4, 1),
                ("the", "DET", "det", 6, 1),
                ("parcel", "NOUN", "dobj", 4, 1),
                ("to", "ADP", "prep", 4, 1),
                ("the", "DET", "det", 9, 1),
                ("clerk", "NOUN", "pobj", 7, 1),
                (".", "PUNCT", "punct", 4, 1),
                ("Mira", "PROPN", "nsubj", 12, 2),
                ("waits", "VERB", "ROOT", 12, 2),
                (".", "PUNCT", "punct", 12, 2),
            ],
        );
        let candidate = doc(
            "Éva rests.  Mira gives the clerk the parcel.\nMira waits.",
            &[
                ("Éva", "PROPN", "nsubj", 1, 0),
                ("rests", "VERB", "ROOT", 1, 0),
                (".", "PUNCT", "punct", 1, 0),
                ("Mira", "PROPN", "nsubj", 4, 1),
                ("gives", "VERB", "ROOT", 4, 1),
                ("the", "DET", "det", 6, 1),
                ("clerk", "NOUN", "dative", 4, 1),
                ("the", "DET", "det", 8, 1),
                ("parcel", "NOUN", "dobj", 4, 1),
                (".", "PUNCT", "punct", 4, 1),
                ("Mira", "PROPN", "nsubj", 11, 2),
                ("waits", "VERB", "ROOT", 11, 2),
                (".", "PUNCT", "punct", 11, 2),
            ],
        );
        let start = source.tokens[5].start_byte;
        let end = source.tokens[9].end_byte;
        let edits = vec![Edit {
            start_byte: start,
            end_byte: end,
            expected: source.text[start..end].into(),
            replacement: "the clerk the parcel".into(),
        }];
        let c = edits::candidate(
            &source.text,
            RuleIdentity {
                rule: "synthetic",
                rule_version: "v1",
                catalog_version: "v1",
                parser_identity: &source.parser_identity,
            },
            edits,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(c.text, candidate.text);
        let candidate_end = candidate.tokens[8].end_byte;
        let role = |d: &Document, head: usize, start: usize, end: usize| json!({"head_index":head,"span":[d.tokens[start].start_byte,d.tokens[end-1].end_byte],"token_indices":(start..end).collect::<Vec<_>>()});
        let p = json!({"candidate":c,"inverse_edits":[Edit{start_byte:start,end_byte:candidate_end,expected:"the clerk the parcel".into(),replacement:source.text[start..end].into()}],"predicate_index":4,
            "roles":{"agent":role(&source,3,3,4),"theme":role(&source,6,5,7),"recipient":role(&source,9,8,10)},
            "byte_segments":[{"source":[0,start],"candidate":[0,start]},
              {"source":[source.tokens[5].start_byte,source.tokens[6].end_byte],"candidate":[candidate.tokens[7].start_byte,candidate.tokens[8].end_byte]},
              {"source":[source.tokens[8].start_byte,source.tokens[9].end_byte],"candidate":[candidate.tokens[5].start_byte,candidate.tokens[6].end_byte]},
              {"source":[end,source.text.len()],"candidate":[candidate_end,candidate.text.len()]}]});
        (source, candidate, p)
    }

    #[test]
    fn exact_nonzero_utf8_slices_rebase_all_indices_and_patches() {
        let (s, c, p) = pair();
        let r = prepare(&s, &c, &p).unwrap();
        assert_eq!(r["status"], "prepared");
        assert_eq!(r["source_text"], "Mira gives the parcel to the clerk.");
        assert_eq!(r["candidate_text"], "Mira gives the clerk the parcel.");
        assert_eq!(r["source_predicate_span"], json!([5, 10]));
        assert_eq!(r["candidate_predicate_span"], json!([5, 10]));
        assert_eq!(r["length_delta_bytes"], -3);
        for key in ["source_projection", "candidate_projection"] {
            assert_eq!(r[key]["status"], "available");
            let d: Document = serde_json::from_value(r[key]["document"].clone()).unwrap();
            syntax::validate(&d).unwrap();
            assert_eq!(d.tokens[0].i, 0);
            assert_eq!(d.tokens[0].start_byte, 0);
            assert_eq!(d.tokens[0].head, 1);
            assert_eq!(d.sentences[0].root, 1);
        }
        let edits: Vec<Edit> = serde_json::from_value(r["forward_edits"].clone()).unwrap();
        assert_eq!(
            edits::apply(r["source_text"].as_str().unwrap(), &edits).unwrap(),
            r["candidate_text"].as_str().unwrap()
        );
    }

    #[test]
    fn changed_candidate_segmentation_does_not_prevent_preparation() {
        let (s, mut c, p) = pair();
        // Merge the changed sentence with the final sentence, preserving valid edges.
        c.tokens[11].head = 4;
        c.tokens[11].dep = "conj".into();
        for token in &mut c.tokens[10..] {
            token.sentence = 1;
        }
        c.sentences[1].end_byte = c.text.len();
        c.sentences[1].token_end = c.tokens.len();
        c.sentences.pop();
        syntax::validate(&c).unwrap();
        let r = prepare(&s, &c, &p).unwrap();
        assert_eq!(r["status"], "prepared");
        assert_eq!(r["candidate_text"], "Mira gives the clerk the parcel.");
        assert_eq!(r["candidate_projection"]["status"], "unavailable");
        assert_eq!(r["source_projection"]["status"], "available");
    }

    #[test]
    fn projection_does_not_trim_gaps_or_repair_partial_sentences() {
        let (s, _, _) = pair();
        let span = [s.sentences[1].start_byte, s.sentences[1].end_byte];
        assert_eq!(project(&s, span).unwrap()["status"], "available");
        assert_eq!(
            project(&s, [span[0] - 1, span[1]]).unwrap()["status"],
            "unavailable"
        );
        assert_eq!(
            project(&s, [s.tokens[4].start_byte, span[1]]).unwrap()["status"],
            "unavailable"
        );
        assert_eq!(
            project(&s, [0, s.text.len()]).unwrap()["status"],
            "unavailable"
        );
        assert!(project(&s, [1, 3]).is_err()); // Byte one splits initial É.
    }

    #[test]
    fn invalid_hashes_edits_roles_and_segment_bindings_fail_closed() {
        let (s, c, p) = pair();
        let mut changed = p.clone();
        changed["candidate"]["source_sha256"] = json!("wrong");
        assert!(prepare(&s, &c, &changed).is_err());
        changed = p.clone();
        changed["inverse_edits"][0]["replacement"] = json!("wrong");
        assert!(prepare(&s, &c, &changed).is_err());
        changed = p.clone();
        changed["candidate"]["edits"][0]["start_byte"] = json!(0);
        assert!(prepare(&s, &c, &changed).is_err());
        changed = p.clone();
        changed["roles"]["theme"]["head_index"] = json!(3);
        assert!(prepare(&s, &c, &changed).is_err());
        changed = p.clone();
        changed["byte_segments"][1]["candidate"][0] = json!(0);
        assert!(prepare(&s, &c, &changed).is_err());
        let mut other = c.clone();
        other.parser_identity = "another".into();
        assert!(prepare(&s, &other, &p).is_err());
    }

    #[test]
    fn valid_cross_sentence_patch_is_retained_as_unavailable() {
        let (s, c, mut p) = pair();
        p["candidate"]["edits"] = json!([Edit {
            start_byte: 0,
            end_byte: s.text.len(),
            expected: s.text.clone(),
            replacement: c.text.clone()
        }]);
        assert_eq!(
            prepare(&s, &c, &p).unwrap()["reason"],
            "forward_edit_outside_source_predicate_sentence"
        );
    }

    #[test]
    fn repeated_words_cannot_redirect_predicate_correspondence() {
        let (mut s, mut c, mut p) = pair();
        // Change the last predicate to the same surface bytes as the edited predicate.
        for d in [&mut s, &mut c] {
            let index = d.tokens.iter().position(|t| t.text == "waits").unwrap();
            let start = d.tokens[index].start_byte;
            d.text.replace_range(start..start + 5, "gives");
            d.tokens[index].text = "gives".into();
            d.tokens[index].lemma = "gives".into();
        }
        p["candidate"]["text"] = json!(c.text);
        p["candidate"]["source_sha256"] = json!(edits::digest(&s.text));
        p["candidate"]["candidate_sha256"] = json!(edits::digest(&c.text));
        let pspan = [s.tokens[4].start_byte, s.tokens[4].end_byte];
        let wrong = [c.tokens[11].start_byte, c.tokens[11].end_byte];
        p["byte_segments"] = json!([{"source":pspan,"candidate":wrong}]);
        assert!(prepare(&s, &c, &p).is_err());
    }
}

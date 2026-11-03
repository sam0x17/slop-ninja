//! Move a finite, comma-delimited adjunct to the start of its matrix sentence.
use super::*;

pub(super) const RULE: RuleDescriptor = RuleDescriptor {
    id: "front_parenthetical_adjunct",
    version: "parenthetical-adjunct-fronting-v1",
    preconditions: &[
        "A finite as/while/although/though adjunct has its own preceding subject and attaches to the matrix predicate or its subject.",
        "Its contiguous subtree is enclosed by two commas inside one prose sentence; the remaining matrix has a preceding subject and finite predicate.",
        "No nested clause, coordination, quotation, question, newline or uncertain opening capitalization is moved.",
    ],
    risks: &[
        "Review scope, discourse emphasis, attribution and the temporal/causal reading of as or while.",
        "A pronoun may move before its antecedent; inspect coreference in the full document.",
        "Parser attachment and grammatical shape do not establish semantic equivalence or readability.",
    ],
    propose,
};

fn opening(doc: &Document, sentence: &Sentence, matrix_subject: &Token) -> Option<String> {
    let first = &doc.tokens[sentence.token_start];
    if first.text == "I" || first.pos == "PROPN" {
        return Some(first.text.clone());
    }
    // Preserve named noun phrases such as The Hague. Other uncertain openers,
    // including capitalized adjectives, remain outside this operation.
    if [
        "He", "She", "It", "We", "They", "You", "A", "An", "The", "This", "These", "Those",
    ]
    .contains(&first.text.as_str())
        && ["PRON", "DET"].contains(&first.pos.as_str())
        && descendant_of(doc, first.i, matrix_subject.i)
    {
        let proper = doc.tokens.iter().any(|t| {
            t.pos == "PROPN" && t.i <= matrix_subject.i && descendant_of(doc, t.i, matrix_subject.i)
        });
        return Some(if proper {
            first.text.clone()
        } else {
            first.text.to_lowercase()
        });
    }
    None
}

fn front(doc: &Document, clause: &Token) -> Option<Edit> {
    let sentence = &doc.sentences[clause.sentence];
    let raw = &doc.text[sentence.start_byte..sentence.end_byte];
    let matrix = &doc.tokens[sentence.root];
    if clause.dep != "advcl"
        || !finite(doc, clause)
        || !finite(doc, matrix)
        || !prose_context(doc, sentence)
        || raw.contains(['\n', '\r', '\t', '(', ')', ';'])
        || !raw.ends_with('.')
    {
        return None;
    }
    let matrix_subjects: Vec<_> = children(doc, matrix.i).filter(|t| subject(t)).collect();
    let [matrix_subject] = matrix_subjects.as_slice() else {
        return None;
    };
    if matrix_subject.i >= matrix.i || ![matrix.i, matrix_subject.i].contains(&clause.head) {
        return None;
    }
    let subjects: Vec<_> = children(doc, clause.i).filter(|t| subject(t)).collect();
    let [own_subject] = subjects.as_slice() else {
        return None;
    };
    if own_subject.i >= clause.i {
        return None;
    }
    let markers: Vec<_> = children(doc, clause.i)
        .filter(|t| t.dep == "mark")
        .collect();
    let [marker] = markers.as_slice() else {
        return None;
    };
    if !["as", "while", "although", "though"].contains(&marker.text.as_str()) {
        return None;
    }
    let members: Vec<_> = doc.tokens[sentence.token_start..sentence.token_end]
        .iter()
        .filter(|t| !t.is_punct && descendant_of(doc, t.i, clause.i))
        .collect();
    let (first, last) = (*members.first()?, *members.last()?);
    if first.i != marker.i || first.i <= sentence.token_start + 1 {
        return None;
    }
    let before = &doc.tokens[first.i - 1];
    let after = doc.tokens.get(last.i + 1)?;
    if before.text != ","
        || after.text != ","
        || after.sentence != clause.sentence
        || after.i + 1 >= sentence.token_end
        || doc.text[before.end_byte..first.start_byte] != *" "
        || doc.text[last.end_byte..after.start_byte] != *""
        || doc.text[after.end_byte..doc.tokens[after.i + 1].start_byte] != *" "
        || doc.tokens[first.i..=last.i].iter().any(|t| {
            t.is_punct
                || t.is_space
                || !descendant_of(doc, t.i, clause.i)
                || (t.i != clause.i
                    && [
                        "advcl",
                        "ccomp",
                        "xcomp",
                        "relcl",
                        "acl",
                        "acl:relcl",
                        "conj",
                        "cc",
                    ]
                    .contains(&t.dep.as_str()))
        })
    {
        return None;
    }
    let first_matrix = &doc.tokens[sentence.token_start];
    // Retain every byte within the adjunct and the rest of the sentence except
    // the two delimiters, their spaces, and the two guarded initial letters.
    let moved = capitalized(&doc.text[first.start_byte..last.end_byte])?;
    let matrix_opening = opening(doc, sentence, matrix_subject)?;
    let replacement = format!(
        "{moved}, {matrix_opening}{}{}",
        &doc.text[first_matrix.end_byte..before.start_byte],
        &doc.text[after.end_byte..sentence.end_byte]
    );
    Some(patch(
        doc,
        sentence.start_byte,
        sentence.end_byte,
        replacement,
    ))
}

fn propose(doc: &Document) -> Vec<Vec<Edit>> {
    doc.tokens
        .iter()
        .filter_map(|clause| front(doc, clause).map(|e| vec![e]))
        .collect()
}

//! Boundary alternatives with embedded clause contents kept intact.
use super::*;
use std::ops::Range;

const PRECONDITIONS: &[&str] = &[
    "Each span has a finite predicate with a preceding overt nominal or clausal subject, and begins with that subject or its retained connector.",
    "Non-punctuation dependencies remain inside each span except the identified clause-head links and boundary connectors.",
    "No marked subordinate opening, quotation, colon, parentheses, internal newline or uncertain name capitalization crosses the boundary.",
    "Semicolon links may be labeled ccomp, conj or parataxis; that dependency label does not certify discourse independence.",
];
const RISKS: &[&str] = &[
    "Review reporting, conditional and negative scope, shared modifiers, reference, rhythm and emphasis in the full document.",
    "Embedded clauses, modals and negation stay verbatim; punctuation changes can still alter their interpreted scope.",
    "Reparse the result and reject fragments or changed argumentation. This is a grammatical nomination, not semantic approval.",
];

pub(super) const SEMICOLON: RuleDescriptor = RuleDescriptor {
    id: "split_complete_semicolon_clauses",
    version: "complete-semicolon-clauses-v1",
    preconditions: PRECONDITIONS,
    risks: RISKS,
    propose: semicolons,
};
pub(super) const COORDINATION: RuleDescriptor = RuleDescriptor {
    id: "split_complete_coordinate_clauses",
    version: "complete-coordinate-clauses-v1",
    preconditions: PRECONDITIONS,
    risks: RISKS,
    propose: coordination,
};

pub(super) const JOIN: RuleDescriptor = RuleDescriptor {
    id: "join_complete_sentences",
    version: "complete-sentence-join-v1",
    preconditions: &[
        "Two adjacent period-ended sentences in one paragraph each start with an overt subject or retained and/but connector and contain a finite main predicate.",
        "Dependencies and embedded contents stay inside their original sentences; no leading frame, quotation, colon, semicolon, parentheses, code or internal newline is admitted.",
        "The boundary becomes a semicolon. Only a recognized ordinary opener is decapitalized; I and initial proper names remain unchanged, and uncertain named phrases are rejected.",
    ],
    risks: &[
        "A semicolon asserts a closer discourse relationship. Review attribution, reporting and negative scope, antecedents, argumentation, rhythm and readability in the whole document.",
        "Words and clause order are retained, but this does not prove that joining the sentences preserves intended meaning or tone.",
        "Adjacent joins may create a long sentence. Review combined candidates; do not infer their readability from individual joins.",
    ],
    propose: joins,
};

fn content(t: &Token) -> bool {
    !t.is_punct && !t.is_space
}
fn usable(doc: &Document, s: &Sentence) -> bool {
    let raw = doc.text[s.start_byte..s.end_byte].trim();
    prose_context(doc, s) && raw.ends_with('.') && !raw.contains(['\n', '\r', '\t', ':', '(', ')'])
}

/// Find one clause head whose other dependencies do not escape this token span.
fn clause(doc: &Document, span: Range<usize>, connector: Option<usize>) -> Option<usize> {
    let tokens: Vec<_> = doc.tokens[span.clone()]
        .iter()
        .filter(|t| content(t) && Some(t.i) != connector)
        .collect();
    let roots: Vec<_> = tokens
        .iter()
        .copied()
        .filter(|t| t.head == t.i || !span.contains(&t.head))
        .collect();
    let [root] = roots.as_slice() else {
        return None;
    };
    if !finite(doc, root) || !["ROOT", "conj", "parataxis", "ccomp"].contains(&root.dep.as_str()) {
        return None;
    }
    let subjects: Vec<_> = children(doc, root.i)
        .filter(|t| subject(t) || ["csubj", "csubjpass", "csubj:pass"].contains(&t.dep.as_str()))
        .collect();
    let [own] = subjects.as_slice() else {
        return None;
    };
    if own.i >= root.i || !span.contains(&own.i) || children(doc, root.i).any(|t| t.dep == "mark") {
        return None;
    }
    // A preposed condition or shared adverbial frame falls outside the subject.
    let first = tokens.first()?;
    if !descendant_of(doc, first.i, own.i) {
        return None;
    }
    // Every subject member stays on this side of the proposed boundary.
    if doc
        .tokens
        .iter()
        .any(|t| content(t) && descendant_of(doc, t.i, own.i) && !span.contains(&t.i))
    {
        return None;
    }
    Some(root.i)
}

fn join_clause(doc: &Document, s: &Sentence) -> Option<usize> {
    if !usable(doc, s) || doc.text[s.start_byte..s.end_byte].contains(';') {
        return None;
    }
    let first = &doc.tokens[s.token_start];
    let connector = (first.dep == "cc" && ["and", "but"].contains(&form(&first.text).as_str()))
        .then_some(first.i);
    let root = clause(doc, s.token_start..s.token_end, connector)?;
    if root != s.root || connector.is_some_and(|i| doc.tokens[i].head != root) {
        return None;
    }
    Some(root)
}

/// Return only the changed initial character, not a patch over unchanged words.
fn join_initial(doc: &Document, s: &Sentence, root: usize) -> Option<Option<Edit>> {
    let first = &doc.tokens[s.token_start];
    let initial = first.text.chars().next()?;
    if first.text == "I" || (first.pos == "PROPN" && initial.is_uppercase()) {
        return Some(None);
    }
    let ordinary = match first.pos.as_str() {
        "PRON" | "DET" => [
            "He", "She", "It", "We", "They", "You", "A", "An", "The", "This", "That", "These",
            "Those", "My", "Our", "His", "Her", "Its", "Their", "Your",
        ]
        .contains(&first.text.as_str()),
        "CCONJ" => ["And", "But"].contains(&first.text.as_str()) && first.dep == "cc",
        _ => false,
    };
    if !ordinary {
        return None;
    }
    // The Hague / The Who have named subject heads. Do not guess whether their
    // determiner belongs to the name. Proper names inside relative clauses do
    // not make an ordinary noun phrase a title.
    if first.pos == "DET" && children(doc, root).any(|t| subject(t) && t.pos == "PROPN") {
        return None;
    }
    Some(Some(patch(
        doc,
        first.start_byte,
        first.start_byte + initial.len_utf8(),
        initial.to_ascii_lowercase().to_string(),
    )))
}

fn joins(doc: &Document) -> Vec<Vec<Edit>> {
    let mut out = vec![];
    for pair in doc.sentences.windows(2) {
        let [left, right] = pair else { unreachable!() };
        if join_clause(doc, left).is_none() {
            continue;
        }
        let Some(root) = join_clause(doc, right) else {
            continue;
        };
        let last = &doc.tokens[left.token_end - 1];
        let first = &doc.tokens[right.token_start];
        if last.text != "." || doc.text[last.end_byte..first.start_byte] != *" " {
            continue;
        }
        let Some(case_edit) = join_initial(doc, right, root) else {
            continue;
        };
        let mut edits = vec![patch(doc, last.start_byte, last.end_byte, ";".into())];
        if let Some(edit) = case_edit {
            edits.push(edit);
        }
        out.push(edits);
    }
    out
}

fn semicolons(doc: &Document) -> Vec<Vec<Edit>> {
    let mut out = vec![];
    for s in &doc.sentences {
        if !usable(doc, s) {
            continue;
        }
        let separators: Vec<_> = doc.tokens[s.token_start..s.token_end]
            .iter()
            .filter(|t| t.text == ";")
            .collect();
        if separators.is_empty() {
            continue;
        }
        let mut spans = vec![];
        let mut start = s.token_start;
        for sep in &separators {
            spans.push(start..sep.i);
            start = sep.i + 1;
        }
        spans.push(start..s.token_end);
        let mut roots = vec![];
        let mut openings = vec![];
        let mut valid = true;
        for span in &spans {
            let Some(first) = doc.tokens[span.clone()].iter().find(|t| content(t)) else {
                valid = false;
                break;
            };
            let connector = (first.dep == "cc"
                && ["and", "but"].contains(&form(&first.text).as_str()))
            .then_some(first.i);
            let Some(root) = clause(doc, span.clone(), connector) else {
                valid = false;
                break;
            };
            if let Some(c) = connector
                && (!["conj", "parataxis", "ROOT", "ccomp"]
                    .contains(&doc.tokens[root].dep.as_str())
                    || doc.tokens[c].head == c)
            {
                valid = false;
                break;
            }
            roots.push(root);
            openings.push(first);
        }
        if !valid || roots.iter().any(|i| !roots.contains(&doc.tokens[*i].head)) {
            continue;
        }
        for (sep, first) in separators.iter().zip(openings.iter().skip(1)) {
            if doc.text[sep.end_byte..first.start_byte] != *" "
                || (first.pos == "PROPN"
                    && first.text.chars().next().is_some_and(char::is_lowercase))
            {
                continue;
            }
            if let Some(cap) = capitalized(&first.text) {
                out.push(vec![patch(
                    doc,
                    sep.start_byte,
                    first.end_byte,
                    format!(". {cap}"),
                )]);
            }
        }
    }
    out
}

fn coordination(doc: &Document) -> Vec<Vec<Edit>> {
    let mut out = vec![];
    for s in &doc.sentences {
        if !usable(doc, s) || doc.text[s.start_byte..s.end_byte].contains(';') {
            continue;
        }
        for cc in &doc.tokens[s.token_start..s.token_end] {
            if cc.dep != "cc"
                || !["and", "but"].contains(&form(&cc.text).as_str())
                || cc.i <= s.token_start
                || cc.i + 1 >= s.token_end
            {
                continue;
            }
            let Some(left) = clause(doc, s.token_start..cc.i, None) else {
                continue;
            };
            let Some(right) = clause(doc, cc.i + 1..s.token_end, None) else {
                continue;
            };
            if doc.tokens[left].head != left
                || doc.tokens[right].head != left
                || doc.tokens[right].dep != "conj"
                || ![left, right].contains(&cc.head)
            {
                continue;
            }
            let previous = &doc.tokens[cc.i - 1];
            let next = &doc.tokens[cc.i + 1];
            if (previous.is_punct && previous.text != ",")
                || next.is_punct
                || next.is_space
                || doc.text[previous.end_byte..cc.start_byte] != *" "
                || doc.text[cc.end_byte..next.start_byte] != *" "
            {
                continue;
            }
            let start = if previous.text == "," {
                previous.start_byte
            } else {
                previous.end_byte
            };
            if let Some(cap) = same_case("And", &form(&cc.text)) {
                out.push(vec![patch(doc, start, cc.end_byte, format!(". {cap}"))]);
            }
        }
    }
    out
}

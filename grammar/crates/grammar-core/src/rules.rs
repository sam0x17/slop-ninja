//! Conservative grammar-licensed possibilities. Syntax is not an equivalence proof.

use crate::edits::{self, Candidate, Edit, RuleIdentity};
use crate::syntax::{self, Document, Sentence, Token};
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

mod clause_boundaries;
mod fronting;

pub const BUILTIN_RULES: &[&str] = &[
    "contract_negative_auxiliary",
    "expand_negative_auxiliary",
    "reduce_present_passive_relative",
    "split_independent_coordination",
    "split_independent_semicolon",
    "join_independent_sentences",
    "front_parenthetical_adjunct",
    "split_complete_semicolon_clauses",
    "split_complete_coordinate_clauses",
    "join_complete_sentences",
];
pub const MAX_CANDIDATES: usize = 256;
pub const LEGACY_CATALOG_VERSION: &str = "grammar-rules-v1";
pub const PREVIOUS_CATALOG_VERSION: &str = "grammar-rules-v2";
pub const COMPLETE_BOUNDARY_CATALOG_VERSION: &str = "grammar-rules-v3";
pub const CATALOG_VERSION: &str = "grammar-rules-v4";

const NEGATIVE_FORMS: &[(&str, &str)] = &[
    ("is not", "isn't"),
    ("are not", "aren't"),
    ("was not", "wasn't"),
    ("were not", "weren't"),
    ("has not", "hasn't"),
    ("have not", "haven't"),
    ("had not", "hadn't"),
    ("do not", "don't"),
    ("does not", "doesn't"),
    ("did not", "didn't"),
    ("cannot", "can't"),
    ("could not", "couldn't"),
    ("will not", "won't"),
    ("would not", "wouldn't"),
    ("should not", "shouldn't"),
    ("must not", "mustn't"),
];

fn form(text: &str) -> String {
    text.chars()
        .map(|c| if c == '’' { '\'' } else { c })
        .collect::<String>()
        .to_lowercase()
}

fn same_case(original: &str, replacement: &str) -> Option<String> {
    let letters: Vec<_> = original.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.iter().all(|c| c.is_lowercase()) {
        Some(replacement.into())
    } else if letters.iter().all(|c| c.is_uppercase()) {
        Some(replacement.to_uppercase())
    } else if letters.first().is_some_and(|c| c.is_uppercase())
        && letters[1..].iter().all(|c| c.is_lowercase())
    {
        let mut chars = replacement.chars();
        Some(chars.next()?.to_uppercase().collect::<String>() + chars.as_str())
    } else {
        None
    }
}

fn children(doc: &Document, head: usize) -> impl Iterator<Item = &Token> {
    doc.tokens
        .iter()
        .filter(move |token| token.head == head && token.i != head)
}

fn prose_context(doc: &Document, sentence: &Sentence) -> bool {
    let source = &doc.text[sentence.start_byte..sentence.end_byte];
    if source.contains(['`', '{', '}', '[', ']', '\\', '=', '<', '>'])
        || ["//", "#", "/*"]
            .iter()
            .any(|prefix| source.trim_start().starts_with(prefix))
        || doc.tokens[sentence.token_start..sentence.token_end]
            .iter()
            .any(|token| ["\"", "'", "“", "”", "‘", "’", "``", "''"].contains(&token.text.as_str()))
    {
        return false;
    }
    let prefix = &doc.text[..sentence.start_byte];
    let indentation = prefix.rsplit('\n').next().unwrap_or("");
    if indentation.len() >= 4 && indentation.chars().all(|ch| ch == ' ' || ch == '\t') {
        return false;
    }
    let mut double = false;
    let mut single = false;
    let mut previous = None;
    let mut chars = doc.text.char_indices().peekable();
    while let Some((byte, ch)) = chars.next() {
        if byte >= sentence.start_byte {
            break;
        }
        let next = chars.peek().map(|(_, ch)| *ch);
        let in_word =
            previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric);
        match ch {
            '"' => double = !double,
            '“' => double = true,
            '”' => double = false,
            '‘' if !in_word => single = true,
            '’' if !in_word => single = false,
            '\'' if !in_word && (!previous.is_some_and(char::is_alphanumeric) || single) => {
                single = !single
            }
            _ => {}
        }
        previous = Some(ch);
    }
    !double && !single
}

fn subject(token: &Token) -> bool {
    ["nsubj", "nsubjpass", "nsubj:pass"].contains(&token.dep.as_str())
}

fn auxiliary(token: &Token) -> bool {
    ["aux", "auxpass", "aux:pass", "cop"].contains(&token.dep.as_str())
}

fn finite(doc: &Document, token: &Token) -> bool {
    let finite_token = |token: &Token| {
        ["VBD", "VBP", "VBZ", "MD"].contains(&token.tag.as_str())
            || token
                .morph
                .get("VerbForm")
                .is_some_and(|forms| forms.iter().any(|form| form == "Fin"))
    };
    ["VERB", "AUX"].contains(&token.pos.as_str())
        && (finite_token(token)
            || children(doc, token.i).any(|child| auxiliary(child) && finite_token(child)))
}

fn descendant_of(doc: &Document, token: usize, ancestor: usize) -> bool {
    let mut current = token;
    loop {
        if current == ancestor {
            return true;
        }
        let head = doc.tokens[current].head;
        if head == current {
            return false;
        }
        current = head;
    }
}

fn patch(doc: &Document, start: usize, end: usize, replacement: String) -> Edit {
    Edit {
        start_byte: start,
        end_byte: end,
        expected: doc.text[start..end].into(),
        replacement,
    }
}

fn negative_clause_is_declarative_or_imperative(doc: &Document, aux: &Token, neg: &Token) -> bool {
    if aux.pos != "AUX" || neg.dep != "neg" {
        return false;
    }
    let clause = if auxiliary(aux) { aux.head } else { aux.i };
    if neg.head != clause && neg.head != aux.i {
        return false;
    }
    let subjects: Vec<_> = children(doc, clause)
        .filter(|token| subject(token))
        .collect();
    if !subjects.is_empty() {
        // Literal expansion in an inverted question would give "Do not you...".
        return subjects.iter().all(|subject| subject.i < aux.i);
    }
    let root = &doc.tokens[clause];
    form(&aux.lemma) == "do" && root.dep == "ROOT" && root.pos == "VERB" && root.tag == "VB"
}

fn negative_edit(doc: &Document, index: usize, expand: bool) -> Option<Edit> {
    let aux = doc.tokens.get(index)?;
    let neg = doc.tokens.get(index + 1)?;
    if !prose_context(doc, &doc.sentences[aux.sentence]) {
        return None;
    }
    if aux.sentence != neg.sentence || !negative_clause_is_declarative_or_imperative(doc, aux, neg)
    {
        return None;
    }
    let observed = &doc.text[aux.start_byte..neg.end_byte];
    if observed
        .chars()
        .filter(|ch| ch.is_alphabetic())
        .all(|ch| ch.is_uppercase())
        || doc.text[doc.sentences[aux.sentence].start_byte..doc.sentences[aux.sentence].end_byte]
            .ends_with('!')
    {
        return None;
    }
    let normalized = form(observed);
    let replacement = NEGATIVE_FORMS.iter().find_map(|(full, contracted)| {
        if expand && normalized == *contracted {
            Some(*full)
        } else if !expand && normalized == *full {
            Some(*contracted)
        } else {
            None
        }
    })?;
    // No ambiguous 's/'d, negative inversion, or spaced "can not" rule exists.
    same_case(observed, replacement)
        .map(|replacement| patch(doc, aux.start_byte, neg.end_byte, replacement))
}

fn passive_relative_edit(doc: &Document, index: usize) -> Option<Edit> {
    let relative = doc.tokens.get(index)?;
    if !prose_context(doc, &doc.sentences[relative.sentence]) {
        return None;
    }
    let aux = doc.tokens.get(index + 1)?;
    let participle = doc.tokens.get(index + 2)?;
    if !["that", "which"].contains(&form(&relative.text).as_str())
        || !["nsubjpass", "nsubj:pass"].contains(&relative.dep.as_str())
        || !["is", "are"].contains(&form(&aux.text).as_str())
        || aux.pos != "AUX"
        || !["auxpass", "aux:pass"].contains(&aux.dep.as_str())
        || !["VBP", "VBZ"].contains(&aux.tag.as_str())
        || participle.pos != "VERB"
        || participle.tag != "VBN"
        || !["relcl", "acl:relcl"].contains(&participle.dep.as_str())
        || relative.head != participle.i
        || aux.head != participle.i
        || relative.sentence != participle.sentence
    {
        return None;
    }
    let antecedent = &doc.tokens[participle.head];
    if !["NOUN", "PROPN"].contains(&antecedent.pos.as_str()) || antecedent.i >= relative.i {
        return None;
    }
    if doc.tokens[antecedent.i + 1..relative.i]
        .iter()
        .any(|token| token.is_punct)
        || doc.text[relative.end_byte..aux.start_byte] != *" "
        || doc.text[aux.end_byte..participle.start_byte] != *" "
    {
        return None;
    }
    // The finite tense belongs to is/are, not the VBN's morphological Past/Perf.
    if children(doc, participle.i).any(|child| auxiliary(child) && child.i != aux.i) {
        return None;
    }
    if doc
        .tokens
        .iter()
        .filter(|token| descendant_of(doc, token.i, participle.i))
        .any(|token| {
            token.dep == "neg"
                || token.tag == "MD"
                || ["conj", "advcl", "ccomp", "xcomp"].contains(&token.dep.as_str())
        })
    {
        return None;
    }
    Some(patch(
        doc,
        relative.start_byte,
        participle.start_byte,
        String::new(),
    ))
}

fn plain_scope(doc: &Document, sentence: &Sentence) -> bool {
    prose_context(doc, sentence)
        && doc.tokens[sentence.token_start..sentence.token_end]
            .iter()
            .all(|token| {
                ![
                    "mark",
                    "advcl",
                    "ccomp",
                    "xcomp",
                    "csubj",
                    "csubjpass",
                    "relcl",
                    "acl",
                    "acl:relcl",
                    "neg",
                ]
                .contains(&token.dep.as_str())
                    && token.tag != "MD"
                    && ![
                        "if",
                        "unless",
                        "whether",
                        "provided",
                        "providing",
                        "either",
                        "neither",
                    ]
                    .contains(&form(&token.text).as_str())
                    && !["\"", "'", "“", "”", "‘", "’"].contains(&token.text.as_str())
            })
}

fn plain_predicate(doc: &Document, predicate: &Token) -> bool {
    finite(doc, predicate)
        && children(doc, predicate.i).any(|token| subject(token) && token.i < predicate.i)
        // Prepositional/adverbial material can have shared scope that a basic
        // dependency tree does not expose. Refuse it rather than infer scope.
        && !children(doc, predicate.i).any(|token| ["advmod", "advcl", "prep", "obl", "npadvmod", "parataxis"].contains(&token.dep.as_str()))
}

fn own_subject_start(doc: &Document, predicate: &Token) -> Option<usize> {
    let subject = children(doc, predicate.i).find(|token| subject(token))?;
    doc.tokens
        .iter()
        .filter(|token| descendant_of(doc, token.i, subject.i))
        .map(|token| token.i)
        .min()
}

fn coordinated_split(doc: &Document, sentence: &Sentence) -> Option<Edit> {
    if !plain_scope(doc, sentence) {
        return None;
    }
    let root = &doc.tokens[sentence.root];
    let conjuncts: Vec<_> = children(doc, root.i)
        .filter(|token| token.dep == "conj" && ["VERB", "AUX"].contains(&token.pos.as_str()))
        .collect();
    if conjuncts.len() != 1 {
        return None;
    }
    let conjunct = conjuncts[0];
    if !plain_predicate(doc, root)
        || !plain_predicate(doc, conjunct)
        || children(doc, conjunct.i).any(|token| token.dep == "conj")
    {
        return None;
    }
    let second_start = own_subject_start(doc, conjunct)?;
    let connectors: Vec<_> = doc.tokens[sentence.token_start..sentence.token_end]
        .iter()
        .filter(|token| {
            token.dep == "cc"
                && [root.i, conjunct.i].contains(&token.head)
                && ["and", "but"].contains(&form(&token.text).as_str())
                && token.i > root.i
                && token.i < second_start
        })
        .collect();
    if connectors.len() != 1 {
        return None;
    }
    let connector = connectors[0];
    if doc.text[connector.end_byte..doc.tokens[second_start].start_byte] != *" " {
        return None;
    }
    let previous = doc.tokens.get(connector.i.checked_sub(1)?)?;
    let start = if previous.text == "," {
        previous.start_byte
    } else {
        previous.end_byte
    };
    if previous.is_punct && previous.text != "," {
        return None;
    }
    let gap_start = if previous.text == "," {
        previous.end_byte
    } else {
        start
    };
    if doc.text[gap_start..connector.start_byte] != *" " {
        return None;
    }
    let capitalized = same_case("And", &form(&connector.text))?;
    Some(patch(
        doc,
        start,
        connector.end_byte,
        format!(". {capitalized}"),
    ))
}

fn capitalized(text: &str) -> Option<String> {
    let first = text.chars().next()?;
    if first.is_uppercase() {
        return Some(text.into());
    }
    if !first.is_ascii_lowercase() {
        return None;
    }
    Some(first.to_ascii_uppercase().to_string() + &text[first.len_utf8()..])
}

fn semicolon_split(doc: &Document, sentence: &Sentence) -> Option<Edit> {
    if !plain_scope(doc, sentence) {
        return None;
    }
    let root = &doc.tokens[sentence.root];
    let secondary: Vec<_> = children(doc, root.i)
        .filter(|token| {
            ["parataxis", "conj"].contains(&token.dep.as_str())
                && ["VERB", "AUX"].contains(&token.pos.as_str())
        })
        .collect();
    if secondary.len() != 1 {
        return None;
    }
    let other = secondary[0];
    // Ignore the one explicitly recognized parataxis edge for the scope guard.
    let safe_root = finite(doc, root)
        && children(doc, root.i).any(|token| subject(token) && token.i < root.i)
        && !children(doc, root.i).any(|token| {
            ["advmod", "advcl", "prep", "obl", "npadvmod"].contains(&token.dep.as_str())
        });
    if !safe_root || !plain_predicate(doc, other) || other.i <= root.i {
        return None;
    }
    let start = own_subject_start(doc, other)?;
    let first = &doc.tokens[start];
    let semicolons: Vec<_> = doc.tokens[sentence.token_start..sentence.token_end]
        .iter()
        .filter(|token| token.text == ";")
        .collect();
    if semicolons.len() != 1 {
        return None;
    }
    let separator = semicolons[0];
    if separator.i <= root.i
        || separator.i >= start
        || doc.text[separator.end_byte..first.start_byte] != *" "
        || doc.tokens[sentence.token_start..sentence.token_end]
            .iter()
            .any(|token| token.dep == "cc")
        || (first.pos == "PROPN" && first.text.chars().next().is_some_and(|c| c.is_lowercase()))
    {
        return None;
    }
    Some(patch(
        doc,
        separator.start_byte,
        first.end_byte,
        format!(". {}", capitalized(&first.text)?),
    ))
}

fn sentence_join(doc: &Document, left: &Sentence, right: &Sentence) -> Option<Vec<Edit>> {
    if !plain_scope(doc, left) || !plain_scope(doc, right) {
        return None;
    }
    let lroot = &doc.tokens[left.root];
    let rroot = &doc.tokens[right.root];
    if !plain_predicate(doc, lroot)
        || !plain_predicate(doc, rroot)
        || doc.tokens[left.token_start..right.token_end]
            .iter()
            .any(|token| ["cc", "conj", "parataxis"].contains(&token.dep.as_str()))
    {
        return None;
    }
    let last = &doc.tokens[left.token_end.checked_sub(1)?];
    let first = &doc.tokens[right.token_start];
    if last.text != "."
        || doc.text[last.end_byte..first.start_byte] != *" "
        || own_subject_start(doc, rroot)? != first.i
    {
        return None;
    }
    // Only decapitalize unambiguous ordinary pronouns/determiners. Protect I,
    // proper names and named phrases such as The Hague; reject uncertain openers.
    let lowerable = [
        "He", "She", "It", "We", "They", "You", "A", "An", "The", "This", "These", "Those",
    ]
    .contains(&first.text.as_str())
        && ["PRON", "DET"].contains(&first.pos.as_str());
    let subject_root = children(doc, rroot.i).find(|token| subject(token))?;
    let proper_subject = doc
        .tokens
        .iter()
        .any(|token| token.pos == "PROPN" && descendant_of(doc, token.i, subject_root.i));
    let replacement = if lowerable && !proper_subject {
        first.text.to_lowercase()
    } else if first.text == "I" || first.pos == "PROPN" || (lowerable && proper_subject) {
        first.text.clone()
    } else {
        return None;
    };
    Some(vec![
        patch(doc, last.start_byte, last.end_byte, ";".into()),
        patch(doc, first.start_byte, first.end_byte, replacement),
    ])
}

const NEGATIVE_PRECONDITIONS: &[&str] = &[
    "An unambiguous dictionary form spans adjacent auxiliary and negation tokens.",
    "Negation attaches to the same auxiliary clause; a subject precedes the auxiliary, or the clause is an explicit do-imperative.",
    "No inverted question, ambiguous 's/'d/ain't, spaced can not, explicit all-caps emphasis, quotation or recognized code delimiters.",
];
const NEGATIVE_RISKS: &[&str] = &[
    "Review negation emphasis, pragmatic force, register and tone; contraction is not a semantic certificate.",
    "Unmarked code, quotation and emphasis cannot be ruled out by surface guards.",
];
const RELATIVE_PRECONDITIONS: &[&str] = &[
    "A restrictive that/which subject-relative contains adjacent is/are and a passive participle modifying a noun.",
    "Exactly one present passive auxiliary; no relative-clause negation, modal, additional auxiliary or embedded coordination/clause.",
];
const RELATIVE_RISKS: &[&str] = &[
    "Removing explicit present tense can change temporal interpretation, aspect or attribution.",
    "Review attachment, restrictive reading and every qualification; no inverse tense is guessed.",
];
const BOUNDARY_PRECONDITIONS: &[&str] = &[
    "Both predicates have their own preceding overt subjects and finite syntax.",
    "No recognized subordinate, conditional, modal, negative, reporting or shared adverbial/prepositional scope.",
    "Logical conjunction words are retained and sentence-opening capitalization is guarded.",
];
const BOUNDARY_RISKS: &[&str] = &[
    "Dependency independence does not prove semantic independence or discourse equivalence.",
    "Review scope, emphasis, rhythm, causal/contrast relations and attribution after changing sentence boundaries.",
];

/// A rule proposes patches against validated syntax; patch application and
/// provenance remain independent of the parser implementation.
pub trait RewriteRule {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn apply(&self, doc: &Document) -> Result<Vec<Vec<Edit>>>;
}

#[derive(Clone, Copy)]
pub struct RuleDescriptor {
    pub id: &'static str,
    pub version: &'static str,
    pub preconditions: &'static [&'static str],
    pub risks: &'static [&'static str],
    pub propose: fn(&Document) -> Vec<Vec<Edit>>,
}

impl RewriteRule for RuleDescriptor {
    fn id(&self) -> &str {
        self.id
    }
    fn version(&self) -> &str {
        self.version
    }
    fn apply(&self, doc: &Document) -> Result<Vec<Vec<Edit>>> {
        syntax::validate(doc)?;
        let proposals = (self.propose)(doc);
        for patches in &proposals {
            edits::apply(&doc.text, patches)?;
        }
        Ok(proposals)
    }
}

fn contract_negative(doc: &Document) -> Vec<Vec<Edit>> {
    (0..doc.tokens.len())
        .filter_map(|i| negative_edit(doc, i, false).map(|edit| vec![edit]))
        .collect()
}
fn expand_negative(doc: &Document) -> Vec<Vec<Edit>> {
    (0..doc.tokens.len())
        .filter_map(|i| negative_edit(doc, i, true).map(|edit| vec![edit]))
        .collect()
}
fn reduce_relative(doc: &Document) -> Vec<Vec<Edit>> {
    (0..doc.tokens.len())
        .filter_map(|i| passive_relative_edit(doc, i).map(|edit| vec![edit]))
        .collect()
}
fn split_coordination(doc: &Document) -> Vec<Vec<Edit>> {
    doc.sentences
        .iter()
        .filter_map(|sentence| coordinated_split(doc, sentence).map(|edit| vec![edit]))
        .collect()
}
fn split_semicolon(doc: &Document) -> Vec<Vec<Edit>> {
    doc.sentences
        .iter()
        .filter_map(|sentence| semicolon_split(doc, sentence).map(|edit| vec![edit]))
        .collect()
}
fn join_sentences(doc: &Document) -> Vec<Vec<Edit>> {
    doc.sentences
        .windows(2)
        .filter_map(|pair| sentence_join(doc, &pair[0], &pair[1]))
        .collect()
}

pub static CATALOG: &[RuleDescriptor] = &[
    RuleDescriptor {
        id: "contract_negative_auxiliary",
        version: "negative-aux-v1",
        preconditions: NEGATIVE_PRECONDITIONS,
        risks: NEGATIVE_RISKS,
        propose: contract_negative,
    },
    RuleDescriptor {
        id: "expand_negative_auxiliary",
        version: "negative-aux-v1",
        preconditions: NEGATIVE_PRECONDITIONS,
        risks: NEGATIVE_RISKS,
        propose: expand_negative,
    },
    RuleDescriptor {
        id: "reduce_present_passive_relative",
        version: "passive-relative-v1",
        preconditions: RELATIVE_PRECONDITIONS,
        risks: RELATIVE_RISKS,
        propose: reduce_relative,
    },
    RuleDescriptor {
        id: "split_independent_coordination",
        version: "independent-coordination-v1",
        preconditions: BOUNDARY_PRECONDITIONS,
        risks: BOUNDARY_RISKS,
        propose: split_coordination,
    },
    RuleDescriptor {
        id: "split_independent_semicolon",
        version: "independent-semicolon-v1",
        preconditions: BOUNDARY_PRECONDITIONS,
        risks: BOUNDARY_RISKS,
        propose: split_semicolon,
    },
    RuleDescriptor {
        id: "join_independent_sentences",
        version: "independent-sentence-join-v1",
        preconditions: BOUNDARY_PRECONDITIONS,
        risks: BOUNDARY_RISKS,
        propose: join_sentences,
    },
    fronting::RULE,
    clause_boundaries::SEMICOLON,
    clause_boundaries::COORDINATION,
    clause_boundaries::JOIN,
];

pub fn generate(doc: &Document, max_candidates: usize) -> Result<Vec<Candidate>> {
    generate_selected(
        doc,
        max_candidates,
        &BUILTIN_RULES
            .iter()
            .map(|rule| (*rule).into())
            .collect::<Vec<_>>(),
    )
}

/// Round-robin over selected rules, preserving left-to-right order within each.
pub fn generate_selected(
    doc: &Document,
    max_candidates: usize,
    selected: &[String],
) -> Result<Vec<Candidate>> {
    generate_selected_at_version(doc, max_candidates, selected, CATALOG_VERSION)
}

/// Replay earlier catalogs without adding rules or changing their identities.
pub fn generate_selected_at_version(
    doc: &Document,
    max_candidates: usize,
    selected: &[String],
    catalog_version: &str,
) -> Result<Vec<Candidate>> {
    syntax::validate(doc)?;
    ensure!(
        [
            LEGACY_CATALOG_VERSION,
            PREVIOUS_CATALOG_VERSION,
            COMPLETE_BOUNDARY_CATALOG_VERSION,
            CATALOG_VERSION
        ]
        .contains(&catalog_version),
        "Unknown rewrite catalog"
    );
    ensure!(
        (1..=MAX_CANDIDATES).contains(&max_candidates),
        "Candidate limit must be between 1 and {MAX_CANDIDATES}"
    );
    let requested: BTreeSet<_> = selected.iter().map(String::as_str).collect();
    ensure!(
        requested.len() == selected.len(),
        "Duplicate rule selection"
    );
    ensure!(
        requested.iter().all(|rule| BUILTIN_RULES.contains(rule)),
        "Unknown rewrite rule"
    );
    let available = match catalog_version {
        LEGACY_CATALOG_VERSION => &BUILTIN_RULES[..6],
        PREVIOUS_CATALOG_VERSION => &BUILTIN_RULES[..7],
        COMPLETE_BOUNDARY_CATALOG_VERSION => &BUILTIN_RULES[..9],
        _ => BUILTIN_RULES,
    };
    ensure!(
        requested.iter().all(|rule| available.contains(rule)),
        "Rewrite rule is unavailable in the requested catalog"
    );
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let mut buckets = CATALOG
        .iter()
        .filter(|rule| requested.contains(rule.id))
        .map(|rule| Ok((rule, rule.apply(doc)?.into_iter())))
        .collect::<Result<Vec<_>>>()?;
    loop {
        let mut progressed = false;
        for (rule, proposals) in &mut buckets {
            let Some(patches) = proposals.next() else {
                continue;
            };
            progressed = true;
            let identity = RuleIdentity {
                rule: rule.id,
                rule_version: rule.version,
                catalog_version,
                parser_identity: &doc.parser_identity,
            };
            let candidate =
                edits::candidate(&doc.text, identity, patches, rule.preconditions, rule.risks)?;
            if seen.insert(candidate.candidate_sha256.clone()) {
                output.push(candidate);
                if output.len() == max_candidates {
                    return Ok(output);
                }
            }
        }
        if !progressed {
            break;
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type Spec<'a> = (&'a str, &'a str, &'a str, usize, &'a str);

    fn fixture(text: &str, specs: &[Spec<'_>], ends: &[usize]) -> Document {
        let mut cursor = 0;
        let tokens: Vec<_> = specs
            .iter()
            .enumerate()
            .map(|(i, (word, pos, dep, head, tag))| {
                let start = cursor + text[cursor..].find(word).unwrap();
                cursor = start + word.len();
                Token {
                    i,
                    start_byte: start,
                    end_byte: cursor,
                    text: (*word).into(),
                    lemma: form(word),
                    pos: (*pos).into(),
                    tag: (*tag).into(),
                    dep: (*dep).into(),
                    head: *head,
                    sentence: ends.iter().position(|end| i < *end).unwrap(),
                    morph: BTreeMap::new(),
                    is_punct: *pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect();
        let mut start = 0;
        let sentences = ends
            .iter()
            .map(|end| {
                let root = (start..*end).find(|i| tokens[*i].head == *i).unwrap();
                let sentence = Sentence {
                    start_byte: tokens[start].start_byte,
                    end_byte: tokens[*end - 1].end_byte,
                    root,
                    token_start: start,
                    token_end: *end,
                };
                start = *end;
                sentence
            })
            .collect();
        let doc = Document {
            text: text.into(),
            parser_identity: "synthetic-fixture-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&doc).unwrap();
        doc
    }

    fn selected(doc: &Document, rule: &str) -> Vec<Candidate> {
        generate_selected(doc, 20, &[rule.into()]).unwrap()
    }

    #[test]
    fn complete_join_preserves_embedded_words_and_legacy_catalogs() {
        let entries = [
            ("The", "DET", "det", 1, "DT"),
            ("café", "NOUN", "nsubj", 5, "NN"),
            ("that", "PRON", "dobj", 4, "WDT"),
            ("Mira", "PROPN", "nsubj", 4, "NNP"),
            ("owns", "VERB", "relcl", 1, "VBZ"),
            ("stays", "VERB", "ROOT", 5, "VBZ"),
            ("open", "ADJ", "acomp", 5, "JJ"),
            (".", "PUNCT", "punct", 5, "."),
            ("The", "DET", "det", 9, "DT"),
            ("bakery", "NOUN", "nsubj", 10, "NN"),
            ("serves", "VERB", "ROOT", 10, "VBZ"),
            ("customers", "NOUN", "dobj", 10, "NNS"),
            (".", "PUNCT", "punct", 10, "."),
        ];
        let source = "The café that Mira owns stays open. The bakery serves customers.";
        let doc = fixture(source, &entries, &[8, 13]);
        let rule = "join_complete_sentences";
        let candidates = selected(&doc, rule);
        assert_eq!(candidates.len(), 1);
        let c = &candidates[0];
        assert_eq!(
            c.text,
            "The café that Mira owns stays open; the bakery serves customers."
        );
        assert_eq!(
            c.edits
                .iter()
                .map(|e| e.expected.as_str())
                .collect::<Vec<_>>(),
            [".", "T"]
        );
        assert_eq!(edits::apply(source, &c.edits).unwrap(), c.text);
        assert!(!c.semantic_equivalence_certified);
        assert_eq!(c.meaning_status, edits::MeaningStatus::RequiresReview);
        for version in [
            LEGACY_CATALOG_VERSION,
            PREVIOUS_CATALOG_VERSION,
            COMPLETE_BOUNDARY_CATALOG_VERSION,
        ] {
            assert!(generate_selected_at_version(&doc, 10, &[rule.into()], version).is_err());
            assert!(
                generate_selected_at_version(
                    &doc,
                    10,
                    &["join_independent_sentences".into()],
                    version
                )
                .unwrap()
                .is_empty()
            );
        }
        for separator in ["\n", "\n\n", "  "] {
            let changed = source.replacen(". The", &format!(".{separator}The"), 1);
            assert!(selected(&fixture(&changed, &entries, &[8, 13]), rule).is_empty());
        }
        let mut named = doc.clone();
        named.tokens[9].pos = "PROPN".into();
        assert!(selected(&named, rule).is_empty());
        let mut nonfinite = doc;
        nonfinite.tokens[10].tag = "VBG".into();
        assert!(selected(&nonfinite, rule).is_empty());
    }

    #[test]
    fn complete_boundaries_keep_embedded_clauses_and_reject_escaping_dependencies() {
        let doc = fixture(
            "The café that Mira owns stays open; the bakery serves customers.",
            &[
                ("The", "DET", "det", 1, "DT"),
                ("café", "NOUN", "nsubj", 5, "NN"),
                ("that", "PRON", "dobj", 4, "WDT"),
                ("Mira", "PROPN", "nsubj", 4, "NNP"),
                ("owns", "VERB", "relcl", 1, "VBZ"),
                ("stays", "VERB", "ROOT", 5, "VBZ"),
                ("open", "ADJ", "acomp", 5, "JJ"),
                (";", "PUNCT", "punct", 5, ":"),
                ("the", "DET", "det", 9, "DT"),
                ("bakery", "NOUN", "nsubj", 10, "NN"),
                ("serves", "VERB", "conj", 5, "VBZ"),
                ("customers", "NOUN", "dobj", 10, "NNS"),
                (".", "PUNCT", "punct", 5, "."),
            ],
            &[13],
        );
        let rule = "split_complete_semicolon_clauses";
        let candidate = selected(&doc, rule).remove(0);
        assert_eq!(
            candidate.text,
            "The café that Mira owns stays open. The bakery serves customers."
        );
        assert_eq!(
            edits::apply(&doc.text, &candidate.edits).unwrap(),
            candidate.text
        );
        assert!(!candidate.semantic_equivalence_certified);
        for version in [LEGACY_CATALOG_VERSION, PREVIOUS_CATALOG_VERSION] {
            assert!(generate_selected_at_version(&doc, 10, &[rule.into()], version).is_err());
            assert!(
                generate_selected_at_version(
                    &doc,
                    10,
                    &["split_independent_semicolon".into()],
                    version
                )
                .unwrap()
                .is_empty()
            );
        }
        let mut reverse = doc.clone();
        reverse.tokens[5].head = 10;
        reverse.tokens[5].dep = "ccomp".into();
        reverse.tokens[10].head = 10;
        reverse.tokens[10].dep = "ROOT".into();
        reverse.sentences[0].root = 10;
        assert_eq!(selected(&reverse, rule)[0].text, candidate.text);
        for (index, dep, head, tag) in [
            (11, "dobj", 5, "NNS"),
            (9, "dobj", 10, "NN"),
            (10, "conj", 1, "VBZ"),
            (10, "conj", 5, "VBG"),
        ] {
            let mut bad = doc.clone();
            bad.tokens[index].dep = dep.into();
            bad.tokens[index].head = head;
            bad.tokens[index].tag = tag.into();
            assert!(selected(&bad, rule).is_empty());
        }
    }

    #[test]
    fn adjunct_fronting_preserves_bytes_and_requires_review_of_reference_and_scope() {
        let doc = fixture(
            "\n\nThe café, although Mira dislikes it, remains open.\n",
            &[
                ("The", "DET", "det", 1, "DT"),
                ("café", "NOUN", "nsubj", 8, "NN"),
                (",", "PUNCT", "punct", 8, ","),
                ("although", "SCONJ", "mark", 5, "IN"),
                ("Mira", "PROPN", "nsubj", 5, "NNP"),
                ("dislikes", "VERB", "advcl", 8, "VBZ"),
                ("it", "PRON", "dobj", 5, "PRP"),
                (",", "PUNCT", "punct", 8, ","),
                ("remains", "VERB", "ROOT", 8, "VBZ"),
                ("open", "ADJ", "acomp", 8, "JJ"),
                (".", "PUNCT", "punct", 8, "."),
            ],
            &[11],
        );
        let rule = "front_parenthetical_adjunct";
        let candidates = selected(&doc, rule);
        assert_eq!(candidates.len(), 1);
        let candidate = &candidates[0];
        assert_eq!(
            candidate.text,
            "\n\nAlthough Mira dislikes it, the café remains open.\n"
        );
        assert_eq!(
            edits::apply(&doc.text, &candidate.edits).unwrap(),
            candidate.text
        );
        assert_eq!(
            candidate.meaning_status,
            edits::MeaningStatus::RequiresReview
        );
        assert!(!candidate.semantic_equivalence_certified);
        // A subject-attached parenthetical is also eligible. Its named actor
        // must not capitalize the ordinary matrix determiner.
        let mut subject_attached = doc.clone();
        subject_attached.tokens[5].head = 1;
        assert_eq!(selected(&subject_attached, rule)[0].text, candidate.text);
        for (index, dep, head, tag) in [
            (5, "relcl", 1, "VBZ"),
            (5, "advcl", 8, "VBG"),
            (6, "dobj", 8, "PRP"),
            (4, "dobj", 5, "NNP"),
        ] {
            let mut bad = doc.clone();
            bad.tokens[index].dep = dep.into();
            bad.tokens[index].head = head;
            bad.tokens[index].tag = tag.into();
            assert!(selected(&bad, rule).is_empty());
        }
        assert!(
            generate_selected_at_version(&doc, 20, &[rule.into()], LEGACY_CATALOG_VERSION).is_err()
        );
        let old = negative("Cafe\u{301} is not closed.", "is", "not");
        let current = selected(&old, "contract_negative_auxiliary");
        let legacy = generate_selected_at_version(
            &old,
            20,
            &["contract_negative_auxiliary".into()],
            LEGACY_CATALOG_VERSION,
        )
        .unwrap();
        let mut expected = current[0].clone();
        expected.catalog_version = LEGACY_CATALOG_VERSION.into();
        assert_eq!(legacy, vec![expected]);
    }

    fn negative(text: &str, aux: &str, neg: &str) -> Document {
        fixture(
            text,
            &[
                ("Cafe\u{301}", "PROPN", "nsubj", 1, "NNP"),
                (aux, "AUX", "ROOT", 1, "VBZ"),
                (neg, "PART", "neg", 1, "RB"),
                ("closed", "ADJ", "acomp", 1, "JJ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[5],
        )
    }

    #[test]
    fn negative_forms_are_exact_inverse_candidates_with_unicode_offsets() {
        let original = negative("Cafe\u{301} is not closed.", "is", "not");
        let contracted = selected(&original, "contract_negative_auxiliary");
        assert_eq!(contracted.len(), 1);
        assert_eq!(contracted[0].text, "Cafe\u{301} isn't closed.");
        assert_eq!(contracted[0].edits[0].start_byte, "Cafe\u{301} ".len());
        let parsed = negative(&contracted[0].text, "is", "n't");
        assert_eq!(
            selected(&parsed, "expand_negative_auxiliary")[0].text,
            original.text
        );
        let curly = negative("Cafe\u{301} isn’t closed.", "is", "n’t");
        assert_eq!(
            selected(&curly, "expand_negative_auxiliary")[0].text,
            original.text
        );
        assert_eq!(original.text, "Cafe\u{301} is not closed.");
    }

    #[test]
    fn ordinary_imperative_requires_review_and_explicit_emphasis_is_rejected() {
        let doc = fixture(
            "Do not run.",
            &[
                ("Do", "AUX", "aux", 2, "VBP"),
                ("not", "PART", "neg", 2, "RB"),
                ("run", "VERB", "ROOT", 2, "VB"),
                (".", "PUNCT", "punct", 2, "."),
            ],
            &[4],
        );
        let candidates = selected(&doc, "contract_negative_auxiliary");
        assert_eq!(candidates[0].text, "Don't run.");
        assert!(!candidates[0].semantic_equivalence_certified);
        assert!(candidates[0].risks[0].contains("tone"));
        let emphatic = fixture(
            "DO NOT RUN.",
            &[
                ("DO", "AUX", "aux", 2, "VBP"),
                ("NOT", "PART", "neg", 2, "RB"),
                ("RUN", "VERB", "ROOT", 2, "VB"),
                (".", "PUNCT", "punct", 2, "."),
            ],
            &[4],
        );
        assert!(selected(&emphatic, "contract_negative_auxiliary").is_empty());
    }

    #[test]
    fn quoted_and_delimited_code_contexts_do_not_get_negative_edits() {
        for (open, close) in [("\"", "\""), ("“", "”"), ("`", "`")] {
            let text = format!("{open}She is not ready.{close}");
            let doc = fixture(
                &text,
                &[
                    (open, "PUNCT", "punct", 2, "``"),
                    ("She", "PRON", "nsubj", 2, "PRP"),
                    ("is", "AUX", "ROOT", 2, "VBZ"),
                    ("not", "PART", "neg", 2, "RB"),
                    ("ready", "ADJ", "acomp", 2, "JJ"),
                    (".", "PUNCT", "punct", 2, "."),
                    (close, "PUNCT", "punct", 2, "''"),
                ],
                &[7],
            );
            assert!(selected(&doc, "contract_negative_auxiliary").is_empty());
        }
    }

    #[test]
    fn ambiguous_forms_inversion_and_spaced_can_not_are_rejected() {
        let inverted = fixture(
            "Isn't she ready?",
            &[
                ("Is", "AUX", "ROOT", 0, "VBZ"),
                ("n't", "PART", "neg", 0, "RB"),
                ("she", "PRON", "nsubj", 0, "PRP"),
                ("ready", "ADJ", "acomp", 0, "JJ"),
                ("?", "PUNCT", "punct", 0, "."),
            ],
            &[5],
        );
        assert!(selected(&inverted, "expand_negative_auxiliary").is_empty());
        let ambiguous = fixture(
            "She's ready.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("'s", "AUX", "ROOT", 1, "VBZ"),
                ("ready", "ADJ", "acomp", 1, "JJ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[4],
        );
        assert!(selected(&ambiguous, "expand_negative_auxiliary").is_empty());
        let spaced = fixture(
            "She can not only sing.",
            &[
                ("She", "PRON", "nsubj", 4, "PRP"),
                ("can", "AUX", "aux", 4, "MD"),
                ("not", "PART", "neg", 4, "RB"),
                ("only", "ADV", "advmod", 4, "RB"),
                ("sing", "VERB", "ROOT", 4, "VB"),
                (".", "PUNCT", "punct", 4, "."),
            ],
            &[6],
        );
        assert!(selected(&spaced, "contract_negative_auxiliary").is_empty());
        let cannot = fixture(
            "She cannot sing.",
            &[
                ("She", "PRON", "nsubj", 3, "PRP"),
                ("can", "AUX", "aux", 3, "MD"),
                ("not", "PART", "neg", 3, "RB"),
                ("sing", "VERB", "ROOT", 3, "VB"),
                (".", "PUNCT", "punct", 3, "."),
            ],
            &[5],
        );
        assert_eq!(
            selected(&cannot, "contract_negative_auxiliary")[0].text,
            "She can't sing."
        );
    }

    fn relative(aux: &str, tag: &str) -> Document {
        fixture(
            &format!("The files that {aux} stored remain."),
            &[
                ("The", "DET", "det", 1, "DT"),
                ("files", "NOUN", "nsubj", 5, "NNS"),
                ("that", "PRON", "nsubjpass", 4, "WDT"),
                (aux, "AUX", "auxpass", 4, tag),
                ("stored", "VERB", "relcl", 1, "VBN"),
                ("remain", "VERB", "ROOT", 5, "VBP"),
                (".", "PUNCT", "punct", 5, "."),
            ],
            &[7],
        )
    }

    #[test]
    fn present_passive_relative_uses_finite_auxiliary_tense() {
        let mut doc = relative("are", "VBP");
        doc.tokens[4]
            .morph
            .insert("Tense".into(), vec!["Past".into()]);
        doc.tokens[4]
            .morph
            .insert("Aspect".into(), vec!["Perf".into()]);
        let candidates = selected(&doc, "reduce_present_passive_relative");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].text, "The files stored remain.");
        assert_eq!(candidates[0].edits[0].expected, "that are ");
        assert!(candidates[0].risks[0].contains("temporal"));
        assert!(selected(&relative("were", "VBD"), "reduce_present_passive_relative").is_empty());
    }

    #[test]
    fn negated_modal_and_nonrestrictive_relatives_are_not_reduced() {
        let negated = fixture(
            "The files that are not stored remain.",
            &[
                ("The", "DET", "det", 1, "DT"),
                ("files", "NOUN", "nsubj", 6, "NNS"),
                ("that", "PRON", "nsubjpass", 5, "WDT"),
                ("are", "AUX", "auxpass", 5, "VBP"),
                ("not", "PART", "neg", 5, "RB"),
                ("stored", "VERB", "relcl", 1, "VBN"),
                ("remain", "VERB", "ROOT", 6, "VBP"),
                (".", "PUNCT", "punct", 6, "."),
            ],
            &[8],
        );
        let modal = fixture(
            "The files that may be stored remain.",
            &[
                ("The", "DET", "det", 1, "DT"),
                ("files", "NOUN", "nsubj", 6, "NNS"),
                ("that", "PRON", "nsubjpass", 5, "WDT"),
                ("may", "AUX", "aux", 5, "MD"),
                ("be", "AUX", "auxpass", 5, "VB"),
                ("stored", "VERB", "relcl", 1, "VBN"),
                ("remain", "VERB", "ROOT", 6, "VBP"),
                (".", "PUNCT", "punct", 6, "."),
            ],
            &[8],
        );
        let comma = fixture(
            "The files, which are stored, remain.",
            &[
                ("The", "DET", "det", 1, "DT"),
                ("files", "NOUN", "nsubj", 7, "NNS"),
                (",", "PUNCT", "punct", 1, ","),
                ("which", "PRON", "nsubjpass", 5, "WDT"),
                ("are", "AUX", "auxpass", 5, "VBP"),
                ("stored", "VERB", "relcl", 1, "VBN"),
                (",", "PUNCT", "punct", 5, ","),
                ("remain", "VERB", "ROOT", 7, "VBP"),
                (".", "PUNCT", "punct", 7, "."),
            ],
            &[9],
        );
        for doc in [negated, modal, comma] {
            assert!(selected(&doc, "reduce_present_passive_relative").is_empty());
        }
    }

    fn coordination() -> Document {
        fixture(
            "The guard checks, and the clerk records.",
            &[
                ("The", "DET", "det", 1, "DT"),
                ("guard", "NOUN", "nsubj", 2, "NN"),
                ("checks", "VERB", "ROOT", 2, "VBZ"),
                (",", "PUNCT", "punct", 2, ","),
                ("and", "CCONJ", "cc", 2, "CC"),
                ("the", "DET", "det", 6, "DT"),
                ("clerk", "NOUN", "nsubj", 7, "NN"),
                ("records", "VERB", "conj", 2, "VBZ"),
                (".", "PUNCT", "punct", 2, "."),
            ],
            &[9],
        )
    }

    #[test]
    fn independent_coordination_split_retains_logical_word() {
        let doc = coordination();
        let candidates = selected(&doc, "split_independent_coordination");
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].text,
            "The guard checks. And the clerk records."
        );
        assert_eq!(candidates[0].edits[0].expected, ", and");
    }

    #[test]
    fn shared_subject_conditional_modal_and_negative_coordination_fail() {
        let shared = fixture(
            "She reads and writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                ("and", "CCONJ", "cc", 1, "CC"),
                ("writes", "VERB", "conj", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[5],
        );
        let conditional = fixture(
            "If rain falls, she reads, and he writes.",
            &[
                ("If", "SCONJ", "mark", 2, "IN"),
                ("rain", "NOUN", "nsubj", 2, "NN"),
                ("falls", "VERB", "advcl", 5, "VBZ"),
                (",", "PUNCT", "punct", 5, ","),
                ("she", "PRON", "nsubj", 5, "PRP"),
                ("reads", "VERB", "ROOT", 5, "VBZ"),
                (",", "PUNCT", "punct", 5, ","),
                ("and", "CCONJ", "cc", 5, "CC"),
                ("he", "PRON", "nsubj", 9, "PRP"),
                ("writes", "VERB", "conj", 5, "VBZ"),
                (".", "PUNCT", "punct", 5, "."),
            ],
            &[11],
        );
        let modal = fixture(
            "She may sing, and he writes.",
            &[
                ("She", "PRON", "nsubj", 2, "PRP"),
                ("may", "AUX", "aux", 2, "MD"),
                ("sing", "VERB", "ROOT", 2, "VB"),
                (",", "PUNCT", "punct", 2, ","),
                ("and", "CCONJ", "cc", 2, "CC"),
                ("he", "PRON", "nsubj", 6, "PRP"),
                ("writes", "VERB", "conj", 2, "VBZ"),
                (".", "PUNCT", "punct", 2, "."),
            ],
            &[8],
        );
        let negative = fixture(
            "She does not sing, and he writes.",
            &[
                ("She", "PRON", "nsubj", 3, "PRP"),
                ("does", "AUX", "aux", 3, "VBZ"),
                ("not", "PART", "neg", 3, "RB"),
                ("sing", "VERB", "ROOT", 3, "VB"),
                (",", "PUNCT", "punct", 3, ","),
                ("and", "CCONJ", "cc", 3, "CC"),
                ("he", "PRON", "nsubj", 7, "PRP"),
                ("writes", "VERB", "conj", 3, "VBZ"),
                (".", "PUNCT", "punct", 3, "."),
            ],
            &[9],
        );
        for doc in [shared, conditional, modal, negative] {
            assert!(selected(&doc, "split_independent_coordination").is_empty());
        }
    }

    #[test]
    fn semicolon_and_sentence_boundaries_have_guarded_inverse_forms() {
        let semicolon = fixture(
            "She reads; he writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (";", "PUNCT", "punct", 1, ":"),
                ("he", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "parataxis", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[6],
        );
        assert_eq!(
            selected(&semicolon, "split_independent_semicolon")[0].text,
            "She reads. He writes."
        );
        let sentences = fixture(
            "She reads. He writes.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
                ("He", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "ROOT", 4, "VBZ"),
                (".", "PUNCT", "punct", 4, "."),
            ],
            &[3, 6],
        );
        assert_eq!(
            selected(&sentences, "join_independent_sentences")[0].text,
            semicolon.text
        );
        let mut complement = semicolon.clone();
        complement.tokens[1].head = 4;
        complement.tokens[1].dep = "ccomp".into();
        complement.tokens[4].head = 4;
        complement.tokens[4].dep = "ROOT".into();
        complement.sentences[0].root = 4;
        assert!(selected(&complement, "split_independent_semicolon").is_empty());
    }

    #[test]
    fn sentence_join_protects_names_i_and_uncertain_capitalization() {
        let first_person = fixture(
            "She reads. I write.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
                ("I", "PRON", "nsubj", 4, "PRP"),
                ("write", "VERB", "ROOT", 4, "VBP"),
                (".", "PUNCT", "punct", 4, "."),
            ],
            &[3, 6],
        );
        assert_eq!(
            selected(&first_person, "join_independent_sentences")[0].text,
            "She reads; I write."
        );
        let polish = fixture(
            "She reads. Polish engineers work.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
                ("Polish", "ADJ", "amod", 4, "JJ"),
                ("engineers", "NOUN", "nsubj", 5, "NNS"),
                ("work", "VERB", "ROOT", 5, "VBP"),
                (".", "PUNCT", "punct", 5, "."),
            ],
            &[3, 7],
        );
        assert!(selected(&polish, "join_independent_sentences").is_empty());
        let name = fixture(
            "She reads. The Hague thrives.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
                ("The", "DET", "det", 4, "DT"),
                ("Hague", "PROPN", "nsubj", 5, "NNP"),
                ("thrives", "VERB", "ROOT", 5, "VBZ"),
                (".", "PUNCT", "punct", 5, "."),
            ],
            &[3, 7],
        );
        assert_eq!(
            selected(&name, "join_independent_sentences")[0].text,
            "She reads; The Hague thrives."
        );
    }

    #[test]
    fn results_are_deterministic_bounded_unique_and_reapply_exactly() {
        let doc = fixture(
            "She is not ready, and he is not late.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("is", "AUX", "ROOT", 1, "VBZ"),
                ("not", "PART", "neg", 1, "RB"),
                ("ready", "ADJ", "acomp", 1, "JJ"),
                (",", "PUNCT", "punct", 1, ","),
                ("and", "CCONJ", "cc", 1, "CC"),
                ("he", "PRON", "nsubj", 7, "PRP"),
                ("is", "AUX", "conj", 1, "VBZ"),
                ("not", "PART", "neg", 7, "RB"),
                ("late", "ADJ", "acomp", 7, "JJ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[11],
        );
        let candidates = generate(&doc, 20).unwrap();
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates, generate(&doc, 20).unwrap());
        assert_eq!(generate(&doc, 1).unwrap(), vec![candidates[0].clone()]);
        assert_eq!(
            candidates
                .iter()
                .map(|c| &c.candidate_sha256)
                .collect::<BTreeSet<_>>()
                .len(),
            candidates.len()
        );
        for candidate in candidates {
            assert_eq!(candidate.catalog_version, CATALOG_VERSION);
            assert_eq!(candidate.parser_identity, doc.parser_identity);
            assert!(!candidate.rule_version.is_empty());
            assert_eq!(
                edits::apply(&doc.text, &candidate.edits).unwrap(),
                candidate.text
            );
        }
        assert!(generate(&doc, 0).is_err());
        assert!(generate_selected(&doc, 10, &["invented".into()]).is_err());
        assert!(
            generate_selected(
                &doc,
                10,
                &[BUILTIN_RULES[0].into(), BUILTIN_RULES[0].into()]
            )
            .is_err()
        );
    }

    #[test]
    fn round_robin_reaches_structural_rules_before_more_contractions() {
        let documents = [
            negative("Cafe\u{301} is not closed.", "is", "not"),
            negative("Cafe\u{301} is not closed.", "is", "not"),
            relative("are", "VBP"),
        ];
        let mut combined = Document {
            text: String::new(),
            parser_identity: "synthetic-fixture-v1".into(),
            tokens: Vec::new(),
            sentences: Vec::new(),
        };
        for doc in documents {
            if !combined.text.is_empty() {
                combined.text.push(' ');
            }
            let byte_offset = combined.text.len();
            let token_offset = combined.tokens.len();
            let sentence_offset = combined.sentences.len();
            combined.text.push_str(&doc.text);
            combined
                .tokens
                .extend(doc.tokens.into_iter().map(|mut token| {
                    token.i += token_offset;
                    token.head += token_offset;
                    token.start_byte += byte_offset;
                    token.end_byte += byte_offset;
                    token.sentence += sentence_offset;
                    token
                }));
            combined
                .sentences
                .extend(doc.sentences.into_iter().map(|mut sentence| {
                    sentence.start_byte += byte_offset;
                    sentence.end_byte += byte_offset;
                    sentence.root += token_offset;
                    sentence.token_start += token_offset;
                    sentence.token_end += token_offset;
                    sentence
                }));
        }
        let candidates = generate(&combined, 2).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.rule.as_str())
                .collect::<Vec<_>>(),
            vec![
                "contract_negative_auxiliary",
                "reduce_present_passive_relative"
            ]
        );
        assert_eq!(
            CATALOG.iter().map(|rule| rule.id()).collect::<Vec<_>>(),
            BUILTIN_RULES
        );
        assert_eq!(CATALOG[0].apply(&combined).unwrap().len(), 2);
        assert_eq!(CATALOG[0].version(), "negative-aux-v1");
    }
}

//! Enumerate the frozen negative-auxiliary rules as observed writing choices.
//!
//! These are pre-guard, rule-licensed opportunities. The denominator excludes
//! negatives the frozen proposer cannot handle. Neither an observed choice nor
//! a reversible patch certifies meaning preservation.

use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Candidate, Edit, RuleIdentity},
    rules::{self, RewriteRule},
    syntax::{self, Document, Token},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "slopninja-negative-choice-observation-v1";
pub const CHOICE_FAMILY: &str = "negative_auxiliary_choice_v1";
pub const COVERAGE_LIMITS: &[&str] = &[
    "Only the frozen negative-aux-v1 contraction and expansion rules define accepted opportunities; this is not complete grammar or negation coverage.",
    "The exact frozen rules require an adjacent auxiliary and dependency-negation token, a recognized dictionary form and declarative or explicit do-imperative syntax.",
    "The frozen proposer excludes recognized quotation/code contexts, emphatic capitalization or exclamation, inverted questions, ambiguous 's/'d/ain't and unsupported dictionary forms; its guards are heuristic.",
    "Cannot/can't is supported; spaced can not is excluded by the original rule.",
    "No parser is called for the counterpart. The original source parser annotations define this observation; candidate guard status is not inferred.",
    "A rejected adjacent pair means the frozen rule emitted no proposal. Rejection reasons are not exposed by that rule and are not inferred here.",
    "Direct RuleDescriptor::apply enumerates all proposals; generate_selected and MAX_CANDIDATES are not used. No opportunity cap, sampling or deduplication by text is applied.",
    "Exact reverse byte application is mechanical reversibility, not a parsed inverse-rule or semantic-equivalence claim.",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedForm {
    Contracted,
    Expanded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClauseType {
    Declarative,
    DoImperative,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenIdentity {
    pub token_index: usize,
    pub byte_span: [usize; 2],
    pub text: String,
    /// The unmodified supplied parser lemma, including missingness/placeholders.
    pub parser_lemma: String,
    pub pos: String,
    pub tag: String,
    pub dependency: String,
    pub head_index: usize,
    pub sentence_index: usize,
}

impl From<&Token> for TokenIdentity {
    fn from(token: &Token) -> Self {
        Self {
            token_index: token.i,
            byte_span: [token.start_byte, token.end_byte],
            text: token.text.clone(),
            parser_lemma: token.lemma.clone(),
            pos: token.pos.clone(),
            tag: token.tag.clone(),
            dependency: token.dep.clone(),
            head_index: token.head,
            sentence_index: token.sentence,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Opportunity {
    pub id: String,
    pub observed_form: ObservedForm,
    /// An inflected dictionary auxiliary, e.g. does/did, not its base lemma.
    pub canonical_auxiliary_form: String,
    /// Lowercase expanded dictionary phrase; cannot remains one surface word.
    pub canonical_expanded_form: String,
    pub clause_type: ClauseType,
    /// Collision-free JSON [canonical_auxiliary_form, clause_type].
    pub conditioning_key: String,
    pub auxiliary: TokenIdentity,
    pub negation: TokenIdentity,
    pub clause_head: TokenIdentity,
    pub clause_subjects: Vec<TokenIdentity>,
    pub observed_byte_span: [usize; 2],
    pub observed_text: String,
    /// Complete exact candidate for local annotation/guard use. Keep real text
    /// in ignored per-post traces, not public aggregate reports.
    pub counterpart: Candidate,
    pub reverse_edit: Edit,
    pub semantic_equivalence_certified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionStatus {
    FrozenRuleNoProposal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RejectedPair {
    pub auxiliary: TokenIdentity,
    pub negation: TokenIdentity,
    pub observed_byte_span: [usize; 2],
    pub observed_text: String,
    pub same_sentence: bool,
    pub status: RejectionStatus,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Coverage {
    pub tokens: usize,
    pub auxiliary_tokens: usize,
    pub dependency_negation_tokens: usize,
    pub adjacent_auxiliary_negation_pairs: usize,
    pub accepted_opportunities: usize,
    pub observed_contracted: usize,
    pub observed_expanded: usize,
    pub declarative_opportunities: usize,
    pub do_imperative_opportunities: usize,
    pub rejected_adjacent_pairs: usize,
    pub negation_tokens_without_accepted_opportunity: usize,
    pub counterpart_materializations: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservationReport {
    pub schema: String,
    pub choice_family: String,
    pub parser_identity: String,
    pub source_sha256: String,
    pub source_bytes: usize,
    pub rule_catalog_version: String,
    pub rule_versions: BTreeMap<String, String>,
    pub coverage: Coverage,
    pub opportunities: Vec<Opportunity>,
    pub rejected_adjacent_pairs: Vec<RejectedPair>,
    pub unmatched_negations: Vec<TokenIdentity>,
    pub coverage_limits: Vec<String>,
    pub semantic_equivalence_certified: bool,
}

fn negative_rule_clause<'a>(doc: &'a Document, auxiliary: &Token) -> &'a Token {
    // This identifies the anchor of an already accepted original proposal. It
    // does not supply or broaden the original rule's eligibility checks.
    let index = if matches!(
        auxiliary.dep.as_str(),
        "aux" | "auxpass" | "aux:pass" | "cop"
    ) {
        auxiliary.head
    } else {
        auxiliary.i
    };
    &doc.tokens[index]
}

fn canonical_expansion(value: &str) -> Result<(String, String)> {
    let expanded = value.to_lowercase();
    let auxiliary = if expanded == "cannot" {
        "can"
    } else {
        expanded
            .strip_suffix(" not")
            .context("Accepted rule expansion has an unrecognized dictionary shape")?
    };
    ensure!(
        !auxiliary.is_empty() && auxiliary.bytes().all(|c| c.is_ascii_lowercase()),
        "Accepted rule expansion has an unrecognized auxiliary"
    );
    Ok((auxiliary.into(), expanded))
}

/// Materialize every accepted opportunity from the two original descriptors.
/// Calling `apply` directly avoids the unrelated global candidate limit.
pub fn observe(doc: &Document) -> Result<ObservationReport> {
    syntax::validate(doc)?;
    let source_sha256 = edits::digest(&doc.text);
    let mut opportunities = Vec::new();
    let mut rule_versions = BTreeMap::new();
    let mut accepted_auxiliaries = BTreeSet::new();
    let mut accepted_negations = BTreeSet::new();
    for (rule_name, observed_form) in [
        ("contract_negative_auxiliary", ObservedForm::Expanded),
        ("expand_negative_auxiliary", ObservedForm::Contracted),
    ] {
        let rule = rules::CATALOG
            .iter()
            .find(|rule| rule.id == rule_name)
            .context("Missing frozen negative rule")?;
        ensure!(
            rule.version == "negative-aux-v1",
            "Negative rule version changed"
        );
        rule_versions.insert(rule.id.into(), rule.version.into());
        for patches in rule.apply(doc)? {
            ensure!(
                patches.len() == 1,
                "Negative choice requires exactly one original patch"
            );
            let edit = &patches[0];
            let auxiliary = doc
                .tokens
                .iter()
                .find(|token| token.start_byte == edit.start_byte)
                .context("Proposal does not start at a source token")?;
            let negation = doc
                .tokens
                .get(auxiliary.i + 1)
                .context("Missing adjacent negative token")?;
            ensure!(
                auxiliary.pos == "AUX"
                    && negation.dep == "neg"
                    && negation.end_byte == edit.end_byte
                    && auxiliary.sentence == negation.sentence,
                "Original proposal has an unexpected auxiliary/negation span"
            );
            ensure!(
                accepted_auxiliaries.insert(auxiliary.i) && accepted_negations.insert(negation.i),
                "Repeated or competing observed opportunity"
            );
            let expanded = match observed_form {
                ObservedForm::Contracted => &edit.replacement,
                ObservedForm::Expanded => &edit.expected,
            };
            let (canonical_auxiliary_form, canonical_expanded_form) =
                canonical_expansion(expanded)?;
            let clause = negative_rule_clause(doc, auxiliary);
            let subjects = doc
                .tokens
                .iter()
                .filter(|token| {
                    token.head == clause.i
                        && token.i != clause.i
                        && matches!(token.dep.as_str(), "nsubj" | "nsubjpass" | "nsubj:pass")
                })
                .collect::<Vec<_>>();
            let clause_type = if subjects.is_empty() {
                ClauseType::DoImperative
            } else {
                ClauseType::Declarative
            };
            let conditioning_key =
                serde_json::to_string(&(&canonical_auxiliary_form, clause_type))?;
            let counterpart = edits::candidate(
                &doc.text,
                RuleIdentity {
                    rule: rule.id,
                    rule_version: rule.version,
                    catalog_version: rules::CATALOG_VERSION,
                    parser_identity: &doc.parser_identity,
                },
                patches.clone(),
                rule.preconditions,
                rule.risks,
            )?;
            let reverse_edit = Edit {
                start_byte: edit.start_byte,
                end_byte: edit
                    .start_byte
                    .checked_add(edit.replacement.len())
                    .context("Counterpart byte overflow")?,
                expected: edit.replacement.clone(),
                replacement: edit.expected.clone(),
            };
            ensure!(
                edits::apply(&counterpart.text, std::slice::from_ref(&reverse_edit))? == doc.text,
                "Counterpart reverse patch failed byte restoration"
            );
            let id = edits::digest(&serde_json::to_string(&(
                CHOICE_FAMILY,
                &doc.parser_identity,
                &source_sha256,
                edit.start_byte,
                edit.end_byte,
                rule.id,
                rule.version,
            ))?);
            opportunities.push(Opportunity {
                id,
                observed_form,
                canonical_auxiliary_form,
                canonical_expanded_form,
                clause_type,
                conditioning_key,
                auxiliary: auxiliary.into(),
                negation: negation.into(),
                clause_head: clause.into(),
                clause_subjects: subjects.into_iter().map(Into::into).collect(),
                observed_byte_span: [edit.start_byte, edit.end_byte],
                observed_text: edit.expected.clone(),
                counterpart,
                reverse_edit,
                semantic_equivalence_certified: false,
            });
        }
    }
    opportunities.sort_by_key(|value| (value.observed_byte_span, value.auxiliary.token_index));
    let mut rejected_adjacent_pairs = Vec::new();
    let mut adjacent = 0;
    for pair in doc.tokens.windows(2) {
        let auxiliary = &pair[0];
        let negation = &pair[1];
        if auxiliary.pos == "AUX" && negation.dep == "neg" {
            adjacent += 1;
            if !accepted_auxiliaries.contains(&auxiliary.i) {
                rejected_adjacent_pairs.push(RejectedPair {
                    auxiliary: auxiliary.into(),
                    negation: negation.into(),
                    observed_byte_span: [auxiliary.start_byte, negation.end_byte],
                    observed_text: doc.text[auxiliary.start_byte..negation.end_byte].into(),
                    same_sentence: auxiliary.sentence == negation.sentence,
                    status: RejectionStatus::FrozenRuleNoProposal,
                });
            }
        }
    }
    let unmatched_negations = doc
        .tokens
        .iter()
        .filter(|token| token.dep == "neg" && !accepted_negations.contains(&token.i))
        .map(Into::into)
        .collect::<Vec<_>>();
    let coverage = Coverage {
        tokens: doc.tokens.len(),
        auxiliary_tokens: doc.tokens.iter().filter(|token| token.pos == "AUX").count(),
        dependency_negation_tokens: doc.tokens.iter().filter(|token| token.dep == "neg").count(),
        adjacent_auxiliary_negation_pairs: adjacent,
        accepted_opportunities: opportunities.len(),
        observed_contracted: opportunities
            .iter()
            .filter(|value| value.observed_form == ObservedForm::Contracted)
            .count(),
        observed_expanded: opportunities
            .iter()
            .filter(|value| value.observed_form == ObservedForm::Expanded)
            .count(),
        declarative_opportunities: opportunities
            .iter()
            .filter(|value| value.clause_type == ClauseType::Declarative)
            .count(),
        do_imperative_opportunities: opportunities
            .iter()
            .filter(|value| value.clause_type == ClauseType::DoImperative)
            .count(),
        rejected_adjacent_pairs: rejected_adjacent_pairs.len(),
        negation_tokens_without_accepted_opportunity: unmatched_negations.len(),
        counterpart_materializations: opportunities.len(),
        truncated: false,
    };
    ensure!(
        coverage.accepted_opportunities + coverage.rejected_adjacent_pairs == adjacent
            && accepted_negations.len() + unmatched_negations.len()
                == coverage.dependency_negation_tokens,
        "Opportunity coverage accounting differs"
    );
    Ok(ObservationReport {
        schema: SCHEMA.into(),
        choice_family: CHOICE_FAMILY.into(),
        parser_identity: doc.parser_identity.clone(),
        source_sha256,
        source_bytes: doc.text.len(),
        rule_catalog_version: rules::CATALOG_VERSION.into(),
        rule_versions,
        coverage,
        opportunities,
        rejected_adjacent_pairs,
        unmatched_negations,
        coverage_limits: COVERAGE_LIMITS.iter().map(|s| (*s).into()).collect(),
        semantic_equivalence_certified: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;

    type Spec<'a> = (&'a str, &'a str, &'a str, usize, &'a str);
    fn fixture(text: &str, specs: &[Spec<'_>]) -> Document {
        let mut cursor = 0;
        let tokens = specs
            .iter()
            .enumerate()
            .map(|(i, &(word, pos, dep, head, tag))| {
                let start = cursor + text[cursor..].find(word).unwrap();
                cursor = start + word.len();
                Token {
                    i,
                    start_byte: start,
                    end_byte: cursor,
                    text: word.into(),
                    lemma: word.to_lowercase(),
                    pos: pos.into(),
                    tag: tag.into(),
                    dep: dep.into(),
                    head,
                    sentence: 0,
                    morph: BTreeMap::new(),
                    is_punct: pos == "PUNCT",
                    is_space: false,
                }
            })
            .collect::<Vec<_>>();
        let sentences = if tokens.is_empty() {
            vec![]
        } else {
            vec![Sentence {
                start_byte: tokens[0].start_byte,
                end_byte: tokens.last().unwrap().end_byte,
                root: tokens.iter().find(|t| t.i == t.head).unwrap().i,
                token_start: 0,
                token_end: tokens.len(),
            }]
        };
        let doc = Document {
            text: text.into(),
            parser_identity: "synthetic-negative-choice-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&doc).unwrap();
        doc
    }
    fn negative(text: &str, subject: &str, aux: &str, neg: &str) -> Document {
        fixture(
            text,
            &[
                (subject, "PRON", "nsubj", 3, "PRP"),
                (aux, "AUX", "aux", 3, "VBP"),
                (neg, "PART", "neg", 3, "RB"),
                ("leave", "VERB", "ROOT", 3, "VB"),
                (".", "PUNCT", "punct", 3, "."),
            ],
        )
    }
    #[test]
    fn paired_forms_share_conditional_identity_and_restore_exact_bytes() {
        for (text, aux, neg, label) in [
            ("We do not leave.", "do", "not", ObservedForm::Expanded),
            ("We don't leave.", "do", "n't", ObservedForm::Contracted),
        ] {
            let doc = negative(text, "We", aux, neg);
            let report = observe(&doc).unwrap();
            let opportunity = &report.opportunities[0];
            assert_eq!(report.coverage.accepted_opportunities, 1);
            assert_eq!(opportunity.observed_form, label);
            assert_eq!(opportunity.canonical_auxiliary_form, "do");
            assert_eq!(opportunity.canonical_expanded_form, "do not");
            assert_eq!(opportunity.conditioning_key, "[\"do\",\"declarative\"]");
            assert_eq!(opportunity.clause_subjects.len(), 1);
            assert_eq!(
                edits::apply(&doc.text, &opportunity.counterpart.edits).unwrap(),
                opportunity.counterpart.text
            );
            assert_eq!(
                edits::apply(
                    &opportunity.counterpart.text,
                    std::slice::from_ref(&opportunity.reverse_edit)
                )
                .unwrap(),
                doc.text
            );
            assert!(
                !opportunity.semantic_equivalence_certified
                    && !report.semantic_equivalence_certified
            );
            assert_eq!(opportunity.counterpart.source_sha256, report.source_sha256);
            assert_eq!(
                opportunity.counterpart.candidate_sha256,
                edits::digest(&opportunity.counterpart.text)
            );
        }
    }
    #[test]
    fn unicode_offsets_apostrophes_and_independent_counterparts_are_preserved() {
        let doc = negative("Zoë don’t leave.", "Zoë", "do", "n’t");
        let report = observe(&doc).unwrap();
        let op = &report.opportunities[0];
        assert_eq!(op.observed_byte_span, [5, 12]);
        assert_eq!(op.observed_text, "don’t");
        assert_eq!(op.counterpart.text, "Zoë do not leave.");
        assert_eq!(op.reverse_edit.replacement, "don’t");
        assert_eq!(op.canonical_auxiliary_form, "do");
    }
    #[test]
    fn cannot_uses_can_key_but_spaced_can_not_has_explicit_rejection() {
        for (text, aux, neg) in [
            ("We cannot leave.", "can", "not"),
            ("We can't leave.", "ca", "n't"),
        ] {
            let mut doc = negative(text, "We", aux, neg);
            doc.tokens[1].lemma = "can".into();
            let result = observe(&doc).unwrap();
            assert_eq!(result.opportunities[0].canonical_auxiliary_form, "can");
            assert_eq!(result.opportunities[0].canonical_expanded_form, "cannot");
        }
        let report = observe(&negative("We can not leave.", "We", "can", "not")).unwrap();
        assert!(report.opportunities.is_empty());
        assert_eq!(report.coverage.rejected_adjacent_pairs, 1);
        assert_eq!(report.unmatched_negations.len(), 1);
        assert_eq!(
            report.rejected_adjacent_pairs[0].status,
            RejectionStatus::FrozenRuleNoProposal
        );
    }
    #[test]
    fn declaratives_and_do_imperatives_are_separate_contexts() {
        let doc = fixture(
            "Do not leave.",
            &[
                ("Do", "AUX", "aux", 2, "VB"),
                ("not", "PART", "neg", 2, "RB"),
                ("leave", "VERB", "ROOT", 2, "VB"),
                (".", "PUNCT", "punct", 2, "."),
            ],
        );
        let report = observe(&doc).unwrap();
        let op = &report.opportunities[0];
        assert_eq!(op.clause_type, ClauseType::DoImperative);
        assert_eq!(op.conditioning_key, "[\"do\",\"do_imperative\"]");
        assert!(op.clause_subjects.is_empty());
        assert_eq!(report.coverage.do_imperative_opportunities, 1);
        let doc = fixture(
            "She is not ready.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("is", "AUX", "ROOT", 1, "VBZ"),
                ("not", "PART", "neg", 1, "RB"),
                ("ready", "ADJ", "acomp", 1, "JJ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
        );
        let op = observe(&doc).unwrap().opportunities.remove(0);
        assert_eq!(op.clause_head.token_index, 1);
        assert_eq!(op.clause_type, ClauseType::Declarative);
        assert_eq!(op.canonical_auxiliary_form, "is");
    }
    #[test]
    fn inflected_auxiliary_keys_do_not_collapse_to_lemmas() {
        for aux in ["does", "did"] {
            let mut doc = negative(&format!("We {aux} not leave."), "We", aux, "not");
            doc.tokens[1].lemma = "do".into();
            assert_eq!(
                observe(&doc).unwrap().opportunities[0].canonical_auxiliary_form,
                aux
            );
        }
    }
    #[test]
    fn rejected_and_nonadjacent_negations_are_visible_with_empty_post_support() {
        let inverted = fixture(
            "Do not we leave?",
            &[
                ("Do", "AUX", "aux", 3, "VBP"),
                ("not", "PART", "neg", 3, "RB"),
                ("we", "PRON", "nsubj", 3, "PRP"),
                ("leave", "VERB", "ROOT", 3, "VB"),
                ("?", "PUNCT", "punct", 3, "."),
            ],
        );
        let report = observe(&inverted).unwrap();
        assert_eq!(report.coverage.accepted_opportunities, 0);
        assert_eq!(report.coverage.adjacent_auxiliary_negation_pairs, 1);
        assert_eq!(report.coverage.rejected_adjacent_pairs, 1);
        let emphatic = negative("We DO NOT leave.", "We", "DO", "NOT");
        assert_eq!(
            observe(&emphatic).unwrap().coverage.rejected_adjacent_pairs,
            1
        );
        let separated = fixture(
            "We do really not leave.",
            &[
                ("We", "PRON", "nsubj", 4, "PRP"),
                ("do", "AUX", "aux", 4, "VBP"),
                ("really", "ADV", "advmod", 4, "RB"),
                ("not", "PART", "neg", 4, "RB"),
                ("leave", "VERB", "ROOT", 4, "VB"),
                (".", "PUNCT", "punct", 4, "."),
            ],
        );
        let report = observe(&separated).unwrap();
        assert_eq!(report.coverage.adjacent_auxiliary_negation_pairs, 0);
        assert_eq!(
            report.coverage.negation_tokens_without_accepted_opportunity,
            1
        );
        let empty = fixture("", &[]);
        let report = observe(&empty).unwrap();
        assert_eq!(report.coverage, Coverage::default());
        assert!(!report.coverage_limits.is_empty());
    }
    #[test]
    fn direct_descriptor_enumeration_is_complete_beyond_global_candidate_cap() {
        let one = negative("We do not leave.", "We", "do", "not");
        let count = rules::MAX_CANDIDATES + 3;
        let mut doc = Document {
            text: String::new(),
            parser_identity: one.parser_identity.clone(),
            tokens: vec![],
            sentences: vec![],
        };
        for i in 0..count {
            if i > 0 {
                doc.text.push(' ');
            }
            let offset = doc.text.len();
            let token_offset = doc.tokens.len();
            doc.text.push_str(&one.text);
            for token in &one.tokens {
                let mut t = token.clone();
                t.i += token_offset;
                t.head += token_offset;
                t.start_byte += offset;
                t.end_byte += offset;
                t.sentence = i;
                doc.tokens.push(t);
            }
            let s = &one.sentences[0];
            doc.sentences.push(Sentence {
                start_byte: s.start_byte + offset,
                end_byte: s.end_byte + offset,
                root: s.root + token_offset,
                token_start: s.token_start + token_offset,
                token_end: s.token_end + token_offset,
            });
        }
        let capped = rules::generate_selected(
            &doc,
            rules::MAX_CANDIDATES,
            &["contract_negative_auxiliary".into()],
        )
        .unwrap();
        assert_eq!(capped.len(), rules::MAX_CANDIDATES);
        let report = observe(&doc).unwrap();
        assert_eq!(report.coverage.accepted_opportunities, count);
        assert!(!report.coverage.truncated);
        assert_eq!(
            report
                .opportunities
                .iter()
                .map(|o| &o.id)
                .collect::<BTreeSet<_>>()
                .len(),
            count
        );
        assert!(
            report
                .opportunities
                .windows(2)
                .all(|p| p[0].observed_byte_span < p[1].observed_byte_span)
        );
        assert!(
            report
                .opportunities
                .iter()
                .all(|o| o.counterpart.text.matches("don't").count() == 1)
        );
    }
    #[test]
    fn malformed_source_annotations_fail_before_enumeration() {
        let mut doc = negative("We do not leave.", "We", "do", "not");
        doc.tokens[2].start_byte += 1;
        assert!(observe(&doc).is_err());
        let mut doc = negative("We do not leave.", "We", "do", "not");
        doc.parser_identity.clear();
        assert!(observe(&doc).is_err());
    }
}

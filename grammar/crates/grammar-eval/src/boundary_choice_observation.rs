//! Raw period/semicolon sites and paired eligibility under unchanged rules.
//!
//! Observing a site does not imply that a rule licenses an edit. Even an exact
//! parsed inverse and two preserved guard observations do not certify meaning.
use crate::claim_guard::{self, GuardReport, Outcome};
use anyhow::{Context, Result, ensure};
use grammar_core::{
    edits::{self, Candidate, Edit, RuleIdentity},
    rules::{self, RewriteRule, RuleDescriptor},
    syntax::{self, Document, Token},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "slopninja-boundary-choice-observation-v1";
pub const PAIR_SCHEMA: &str = "slopninja-paired-boundary-eligibility-v1";
pub const CHOICE_FAMILY: &str = "period_semicolon_choice_v1";
pub const COVERAGE_LIMITS: &[&str] = &[
    "Raw semicolon sites include every token whose text is ';'. Raw period sites include each adjacent parser-sentence pair whose left final token text is '.', including arbitrary gaps and paragraph boundaries.",
    "Only the unchanged split_independent_semicolon and join_independent_sentences descriptors define proposals. No new clause, punctuation, whitespace or capitalization eligibility is inferred here.",
    "The original proposers exclude many subordinate, modal, negative, reporting, shared-scope, quotation/code and uncertain-capitalization contexts. Their source-parser eligibility is not complete grammatical coverage.",
    "A raw site with no proposal is retained as frozen_rule_no_proposal. Original descriptors do not expose rejection reasons, so none is inferred.",
    "Direct RuleDescriptor::apply enumerates every forward and inverse proposal without the global MAX_CANDIDATES cap, sampling or text deduplication.",
    "The unchanged claim guard internally uses capped generate_selected when recognizing licensed boundary edits. Later proposals may therefore fail its observations; they remain in the denominator and are not relabeled as proposer rejections.",
    "This module never parses text. The caller must retain counterpart parser failures separately and bind supplied source and counterpart annotations.",
    "Exact full-source restoration must come from the actual inverse rule edits. At least one restoring inverse and both directional observed_preserved guard outcomes are required for paired eligibility.",
    "Paired eligibility is a parser/rule/guard observation, not semantic, discourse, readability or tonal equivalence certification.",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedForm {
    Period,
    Semicolon,
}

impl ObservedForm {
    fn rule(self) -> &'static str {
        match self {
            Self::Period => "join_independent_sentences",
            Self::Semicolon => "split_independent_semicolon",
        }
    }
    fn inverse(self) -> Self {
        match self {
            Self::Period => Self::Semicolon,
            Self::Semicolon => Self::Period,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenIdentity {
    pub token_index: usize,
    pub byte_span: [usize; 2],
    pub text: String,
    pub parser_lemma: String,
    pub pos: String,
    pub tag: String,
    pub dependency: String,
    pub head_index: usize,
    pub sentence_index: usize,
}

impl From<&Token> for TokenIdentity {
    fn from(t: &Token) -> Self {
        Self {
            token_index: t.i,
            byte_span: [t.start_byte, t.end_byte],
            text: t.text.clone(),
            parser_lemma: t.lemma.clone(),
            pos: t.pos.clone(),
            tag: t.tag.clone(),
            dependency: t.dep.clone(),
            head_index: t.head,
            sentence_index: t.sentence,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SentenceIdentity {
    pub sentence_index: usize,
    pub byte_span: [usize; 2],
    pub token_span: [usize; 2],
    pub root: TokenIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceSpan {
    pub byte_span: [usize; 2],
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SiteStatus {
    Proposed,
    FrozenRuleNoProposal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BoundarySite {
    pub id: String,
    pub observed_form: ObservedForm,
    pub boundary: TokenIdentity,
    /// For a semicolon, its containing sentence; for a period, the left one.
    pub left_sentence: SentenceIdentity,
    /// Present only for a raw period site between adjacent parser sentences.
    pub right_sentence: Option<SentenceIdentity>,
    pub left_neighbor: Option<TokenIdentity>,
    pub right_neighbor: Option<TokenIdentity>,
    pub gap_after_boundary: SourceSpan,
    pub proposal_ids: Vec<String>,
    pub status: SiteStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Opportunity {
    pub id: String,
    pub site_id: String,
    pub observed_form: ObservedForm,
    /// Zero-based ordinal in this original descriptor's uncapped output.
    pub rule_proposal_ordinal: usize,
    /// Contains real source-derived text; keep corpus traces in ignored data.
    pub counterpart: Candidate,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Coverage {
    pub raw_semicolon_sites: usize,
    pub raw_period_sites: usize,
    pub raw_sites: usize,
    pub proposed_sites: usize,
    pub rejected_sites: usize,
    pub semicolon_opportunities: usize,
    pub period_opportunities: usize,
    pub opportunities: usize,
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
    pub sites: Vec<BoundarySite>,
    pub opportunities: Vec<Opportunity>,
    pub coverage: Coverage,
    pub coverage_limits: Vec<String>,
    pub semantic_equivalence_certified: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairStatus {
    Eligible,
    NoExactInverse,
    GuardRejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InverseMatch {
    pub inverse_proposal_ordinal: usize,
    /// Actual inverse-rule candidate, whose text equals the complete source.
    pub counterpart: Candidate,
    pub reverse_guard: GuardReport,
    /// Both this match's reverse guard and the forward guard are preserved.
    pub eligible: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairReport {
    pub schema: String,
    pub choice_family: String,
    pub opportunity_id: String,
    pub site_id: String,
    pub observed_form: ObservedForm,
    pub parser_identity: String,
    pub source_sha256: String,
    pub counterpart_sha256: String,
    pub forward_rule: String,
    pub inverse_rule: String,
    pub inverse_rule_version: String,
    pub inverse_proposal_count: usize,
    pub inverse_nonrestoring_count: usize,
    /// One ID per materialized nonrestoring inverse; duplicates are retained.
    pub nonrestoring_inverse_candidate_ids: Vec<String>,
    pub inverse_matches: Vec<InverseMatch>,
    pub passed_inverse_matches: usize,
    pub forward_guard: GuardReport,
    pub eligible: bool,
    pub status: PairStatus,
    pub semantic_equivalence_certified: bool,
}

fn descriptor(form: ObservedForm) -> Result<&'static RuleDescriptor> {
    let r = rules::CATALOG
        .iter()
        .find(|r| r.id == form.rule())
        .context("Missing original boundary descriptor")?;
    let expected = match form {
        ObservedForm::Period => "independent-sentence-join-v1",
        ObservedForm::Semicolon => "independent-semicolon-v1",
    };
    ensure!(r.version == expected, "Boundary descriptor version changed");
    Ok(r)
}

fn materialize(doc: &Document, rule: &RuleDescriptor, patches: Vec<Edit>) -> Result<Candidate> {
    edits::candidate(
        &doc.text,
        RuleIdentity {
            rule: rule.id,
            rule_version: rule.version,
            catalog_version: rules::CATALOG_VERSION,
            parser_identity: &doc.parser_identity,
        },
        patches,
        rule.preconditions,
        rule.risks,
    )
}

fn sentence_identity(doc: &Document, i: usize) -> SentenceIdentity {
    let s = &doc.sentences[i];
    SentenceIdentity {
        sentence_index: i,
        byte_span: [s.start_byte, s.end_byte],
        token_span: [s.token_start, s.token_end],
        root: (&doc.tokens[s.root]).into(),
    }
}

fn site(
    doc: &Document,
    source_sha256: &str,
    token: &Token,
    form: ObservedForm,
    next_sentence: Option<usize>,
) -> Result<BoundarySite> {
    let right = doc.tokens.get(token.i + 1);
    let end = right.map_or(doc.text.len(), |t| t.start_byte);
    Ok(BoundarySite {
        id: edits::digest(&serde_json::to_string(&(
            CHOICE_FAMILY,
            &doc.parser_identity,
            source_sha256,
            form,
            token.i,
            next_sentence,
        ))?),
        observed_form: form,
        boundary: token.into(),
        left_sentence: sentence_identity(doc, token.sentence),
        right_sentence: next_sentence.map(|i| sentence_identity(doc, i)),
        left_neighbor: token
            .i
            .checked_sub(1)
            .and_then(|i| doc.tokens.get(i))
            .map(Into::into),
        right_neighbor: right.map(Into::into),
        gap_after_boundary: SourceSpan {
            byte_span: [token.end_byte, end],
            text: doc.text[token.end_byte..end].into(),
        },
        proposal_ids: Vec::new(),
        status: SiteStatus::FrozenRuleNoProposal,
    })
}

/// Count raw sites first, then associate uncapped original-rule proposals.
pub fn observe(doc: &Document) -> Result<ObservationReport> {
    syntax::validate(doc)?;
    let source_sha256 = edits::digest(&doc.text);
    let mut sites = Vec::new();
    for token in doc.tokens.iter().filter(|t| t.text == ";") {
        sites.push(site(
            doc,
            &source_sha256,
            token,
            ObservedForm::Semicolon,
            None,
        )?);
    }
    for (index, pair) in doc.sentences.windows(2).enumerate() {
        let final_token = &doc.tokens[pair[0].token_end - 1];
        if final_token.text == "." {
            sites.push(site(
                doc,
                &source_sha256,
                final_token,
                ObservedForm::Period,
                Some(index + 1),
            )?);
        }
    }
    sites.sort_by_key(|s| s.boundary.token_index);
    let mut opportunities = Vec::new();
    let mut rule_versions = BTreeMap::new();
    for form in [ObservedForm::Semicolon, ObservedForm::Period] {
        let rule = descriptor(form)?;
        rule_versions.insert(rule.id.into(), rule.version.into());
        for (ordinal, patches) in rule.apply(doc)?.into_iter().enumerate() {
            let counterpart = materialize(doc, rule, patches)?;
            // This checks association only after the original proposer accepts.
            // It does not supply any independent grammatical eligibility.
            let first = counterpart
                .edits
                .first()
                .context("Empty original proposal")?;
            let matching = sites
                .iter()
                .enumerate()
                .filter(|(_, site)| {
                    site.observed_form == form && site.boundary.byte_span[0] == first.start_byte
                })
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            ensure!(
                matching.len() == 1,
                "Original proposal has no unique raw boundary site"
            );
            let site = &mut sites[matching[0]];
            ensure!(
                first.expected.starts_with(&site.boundary.text)
                    && first.end_byte >= site.boundary.byte_span[1],
                "Original proposal does not cover its boundary token"
            );
            let id = edits::digest(&serde_json::to_string(&(
                &site.id,
                rule.id,
                rule.version,
                ordinal,
                &counterpart,
            ))?);
            site.proposal_ids.push(id.clone());
            site.status = SiteStatus::Proposed;
            opportunities.push(Opportunity {
                id,
                site_id: site.id.clone(),
                observed_form: form,
                rule_proposal_ordinal: ordinal,
                counterpart,
            });
        }
    }
    opportunities.sort_by_key(|o| o.counterpart.edits[0].start_byte);
    let coverage = Coverage {
        raw_semicolon_sites: sites
            .iter()
            .filter(|s| s.observed_form == ObservedForm::Semicolon)
            .count(),
        raw_period_sites: sites
            .iter()
            .filter(|s| s.observed_form == ObservedForm::Period)
            .count(),
        raw_sites: sites.len(),
        proposed_sites: sites.iter().filter(|s| !s.proposal_ids.is_empty()).count(),
        rejected_sites: sites.iter().filter(|s| s.proposal_ids.is_empty()).count(),
        semicolon_opportunities: opportunities
            .iter()
            .filter(|o| o.observed_form == ObservedForm::Semicolon)
            .count(),
        period_opportunities: opportunities
            .iter()
            .filter(|o| o.observed_form == ObservedForm::Period)
            .count(),
        opportunities: opportunities.len(),
        counterpart_materializations: opportunities.len(),
        truncated: false,
    };
    Ok(ObservationReport {
        schema: SCHEMA.into(),
        choice_family: CHOICE_FAMILY.into(),
        parser_identity: doc.parser_identity.clone(),
        source_sha256,
        source_bytes: doc.text.len(),
        rule_catalog_version: rules::CATALOG_VERSION.into(),
        rule_versions,
        sites,
        opportunities,
        coverage,
        coverage_limits: COVERAGE_LIMITS.iter().map(|s| (*s).into()).collect(),
        semantic_equivalence_certified: false,
    })
}

/// Evaluate supplied counterpart annotations against the actual inverse rule.
/// The forward opportunity must be exactly reproducible from the source.
pub fn evaluate_counterpart(
    source: &Document,
    opportunity: &Opportunity,
    counterpart: &Document,
) -> Result<PairReport> {
    syntax::validate(counterpart)?;
    ensure!(
        source.parser_identity == counterpart.parser_identity,
        "Counterpart parser identity changed"
    );
    ensure!(
        counterpart.text == opportunity.counterpart.text,
        "Counterpart text differs from proposal"
    );
    let observed = observe(source)?;
    let matching = observed
        .opportunities
        .iter()
        .filter(|o| o.id == opportunity.id)
        .collect::<Vec<_>>();
    ensure!(
        matching.len() == 1 && matching[0] == opportunity,
        "Forward opportunity is not the actual source proposal"
    );
    let forward_guard = claim_guard::compare(source, counterpart, &opportunity.counterpart.edits)?;
    let inverse_rule = descriptor(opportunity.observed_form.inverse())?;
    let proposals = inverse_rule.apply(counterpart)?;
    let inverse_proposal_count = proposals.len();
    let mut inverse_matches = Vec::new();
    let mut nonrestoring_inverse_candidate_ids = Vec::new();
    for (ordinal, patches) in proposals.into_iter().enumerate() {
        let inverse = materialize(counterpart, inverse_rule, patches)?;
        if inverse.text != source.text {
            nonrestoring_inverse_candidate_ids.push(inverse.id);
            continue;
        }
        let reverse_guard = claim_guard::compare(counterpart, source, &inverse.edits)?;
        let eligible = forward_guard.outcome == Outcome::ObservedPreserved
            && reverse_guard.outcome == Outcome::ObservedPreserved;
        inverse_matches.push(InverseMatch {
            inverse_proposal_ordinal: ordinal,
            counterpart: inverse,
            reverse_guard,
            eligible,
        });
    }
    let passed_inverse_matches = inverse_matches.iter().filter(|m| m.eligible).count();
    let eligible = passed_inverse_matches > 0;
    let status = if eligible {
        PairStatus::Eligible
    } else if inverse_matches.is_empty() {
        PairStatus::NoExactInverse
    } else {
        PairStatus::GuardRejected
    };
    Ok(PairReport {
        schema: PAIR_SCHEMA.into(),
        choice_family: CHOICE_FAMILY.into(),
        opportunity_id: opportunity.id.clone(),
        site_id: opportunity.site_id.clone(),
        observed_form: opportunity.observed_form,
        parser_identity: source.parser_identity.clone(),
        source_sha256: observed.source_sha256,
        counterpart_sha256: edits::digest(&counterpart.text),
        forward_rule: opportunity.counterpart.rule.clone(),
        inverse_rule: inverse_rule.id.into(),
        inverse_rule_version: inverse_rule.version.into(),
        inverse_proposal_count,
        inverse_nonrestoring_count: nonrestoring_inverse_candidate_ids.len(),
        nonrestoring_inverse_candidate_ids,
        inverse_matches,
        passed_inverse_matches,
        forward_guard,
        eligible,
        status,
        semantic_equivalence_certified: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammar_core::syntax::Sentence;
    use std::collections::BTreeSet;

    fn document(text: &str, items: &[(&str, &str, &str, usize, &str)], ends: &[usize]) -> Document {
        let mut cursor = 0;
        let mut tokens = Vec::new();
        for (i, &(word, pos, dep, head, tag)) in items.iter().enumerate() {
            let start_byte = cursor + text[cursor..].find(word).unwrap();
            let end_byte = start_byte + word.len();
            tokens.push(Token {
                i,
                start_byte,
                end_byte,
                text: word.into(),
                lemma: word.to_lowercase(),
                pos: pos.into(),
                tag: tag.into(),
                dep: dep.into(),
                head,
                sentence: 0,
                morph: BTreeMap::new(),
                is_punct: pos == "PUNCT",
                is_space: pos == "SPACE",
            });
            cursor = end_byte;
        }
        let mut start = 0;
        let mut sentences = Vec::new();
        for &end in ends {
            for token in &mut tokens[start..end] {
                token.sentence = sentences.len();
            }
            sentences.push(Sentence {
                start_byte: tokens[start].start_byte,
                end_byte: tokens[end - 1].end_byte,
                root: tokens[start..end].iter().find(|t| t.head == t.i).unwrap().i,
                token_start: start,
                token_end: end,
            });
            start = end;
        }
        let doc = Document {
            text: text.into(),
            parser_identity: "boundary-choice-synthetic-v1".into(),
            tokens,
            sentences,
        };
        syntax::validate(&doc).unwrap();
        doc
    }

    fn semicolon(opener: &str, relation: &str) -> Document {
        document(
            &format!("She reads; {opener} writes."),
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (";", "PUNCT", "punct", 1, ":"),
                (opener, "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", relation, 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[6],
        )
    }

    fn periods(gap: &str) -> Document {
        document(
            &format!("She reads.{gap}He writes."),
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
                ("He", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "ROOT", 4, "VBZ"),
                (".", "PUNCT", "punct", 4, "."),
            ],
            &[3, 6],
        )
    }

    #[test]
    fn raw_period_denominator_retains_every_gap_and_final_period_is_not_a_site() {
        for gap in ["", " ", "  ", "\n", "\n\n", "\t", "\r\n"] {
            let doc = periods(gap);
            let report = observe(&doc).unwrap();
            assert_eq!(report.coverage.raw_period_sites, 1, "{gap:?}");
            assert_eq!(report.sites[0].gap_after_boundary.text, gap);
            assert_eq!(
                report.sites[0]
                    .right_sentence
                    .as_ref()
                    .unwrap()
                    .sentence_index,
                1
            );
            assert_eq!(
                report.coverage.period_opportunities,
                usize::from(gap == " ")
            );
            assert_eq!(
                report.coverage.proposed_sites + report.coverage.rejected_sites,
                1
            );
        }
    }

    #[test]
    fn raw_semicolon_sites_and_zero_site_documents_are_retained_without_eligibility_inference() {
        let doc = document(
            "She reads; he writes; they wait.",
            &[
                ("She", "PRON", "nsubj", 1, "PRP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (";", "PUNCT", "punct", 1, ":"),
                ("he", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "conj", 1, "VBZ"),
                (";", "PUNCT", "punct", 1, ":"),
                ("they", "PRON", "nsubj", 7, "PRP"),
                ("wait", "VERB", "conj", 1, "VBP"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[9],
        );
        let report = observe(&doc).unwrap();
        assert_eq!(report.coverage.raw_semicolon_sites, 2);
        assert_eq!(report.coverage.rejected_sites, 2);
        assert!(report.opportunities.is_empty());
        assert!(
            report
                .sites
                .iter()
                .all(|s| s.status == SiteStatus::FrozenRuleNoProposal)
        );
        let empty = observe(&document("\n\n", &[], &[])).unwrap();
        assert_eq!(empty.coverage, Coverage::default());
        assert!(!empty.semantic_equivalence_certified);
    }

    #[test]
    fn source_anchors_are_utf8_bytes_and_original_candidates_are_unchanged() {
        let doc = document(
            "Zoë reads; he writes.",
            &[
                ("Zoë", "PROPN", "nsubj", 1, "NNP"),
                ("reads", "VERB", "ROOT", 1, "VBZ"),
                (";", "PUNCT", "punct", 1, ":"),
                ("he", "PRON", "nsubj", 4, "PRP"),
                ("writes", "VERB", "conj", 1, "VBZ"),
                (".", "PUNCT", "punct", 1, "."),
            ],
            &[6],
        );
        let report = observe(&doc).unwrap();
        let site = &report.sites[0];
        assert_eq!(site.boundary.byte_span[0], doc.text.find(';').unwrap());
        assert_eq!(site.right_neighbor.as_ref().unwrap().text, "he");
        assert_eq!(site.gap_after_boundary.text, " ");
        let rule = descriptor(ObservedForm::Semicolon).unwrap();
        let expected = materialize(&doc, rule, rule.apply(&doc).unwrap()[0].clone()).unwrap();
        assert_eq!(report.opportunities[0].counterpart, expected);
        assert_eq!(expected.text, "Zoë reads. He writes.");
        assert_eq!(site.proposal_ids, vec![report.opportunities[0].id.clone()]);
        assert_eq!(
            serde_json::from_str::<ObservationReport>(&serde_json::to_string(&report).unwrap())
                .unwrap(),
            report
        );
    }

    #[test]
    fn descriptor_enumeration_retains_all_259_proposals_past_global_cap() {
        let count = 260;
        let text = std::iter::repeat_n("She reads.", count)
            .collect::<Vec<_>>()
            .join(" ");
        let mut items = Vec::new();
        let mut ends = Vec::new();
        for i in 0..count {
            let root = i * 3 + 1;
            items.extend([
                ("She", "PRON", "nsubj", root, "PRP"),
                ("reads", "VERB", "ROOT", root, "VBZ"),
                (".", "PUNCT", "punct", root, "."),
            ]);
            ends.push((i + 1) * 3);
        }
        let doc = document(&text, &items, &ends);
        let report = observe(&doc).unwrap();
        assert_eq!(report.coverage.raw_sites, 259);
        assert_eq!(report.coverage.proposed_sites, 259);
        assert_eq!(report.coverage.counterpart_materializations, 259);
        assert!(!report.coverage.truncated);
        assert_eq!(
            report
                .opportunities
                .iter()
                .map(|o| &o.id)
                .collect::<BTreeSet<_>>()
                .len(),
            259
        );
        assert_eq!(
            rules::generate_selected(
                &doc,
                rules::MAX_CANDIDATES,
                &["join_independent_sentences".into()]
            )
            .unwrap()
            .len(),
            256
        );
    }

    #[test]
    fn actual_inverse_patches_and_both_directional_guards_are_required() {
        let source = semicolon("he", "conj");
        let candidate = periods(" ");
        let forward = observe(&source).unwrap().opportunities.remove(0);
        let pair = evaluate_counterpart(&source, &forward, &candidate).unwrap();
        assert_eq!(pair.status, PairStatus::Eligible);
        assert!(pair.eligible);
        assert_eq!(pair.inverse_matches.len(), 1);
        assert_eq!(pair.forward_guard.outcome, Outcome::ObservedPreserved);
        assert_eq!(
            pair.inverse_matches[0].reverse_guard.outcome,
            Outcome::ObservedPreserved
        );
        assert_eq!(forward.counterpart.edits.len(), 1);
        assert_eq!(pair.inverse_matches[0].counterpart.edits.len(), 2);
        assert_eq!(
            edits::apply(&candidate.text, &pair.inverse_matches[0].counterpart.edits).unwrap(),
            source.text
        );
        assert!(!pair.semantic_equivalence_certified);
        let reverse = observe(&candidate).unwrap().opportunities.remove(0);
        let back = evaluate_counterpart(&candidate, &reverse, &source).unwrap();
        assert!(back.eligible);
        assert_eq!(back.inverse_matches[0].counterpart.edits.len(), 1);
    }

    #[test]
    fn nonrestoring_case_changes_and_absent_inverse_are_separate_from_guard_rejection() {
        let source = semicolon("He", "conj");
        let forward = observe(&source).unwrap().opportunities.remove(0);
        let pair = evaluate_counterpart(&source, &forward, &periods(" ")).unwrap();
        assert_eq!(pair.status, PairStatus::NoExactInverse);
        assert_eq!(pair.inverse_proposal_count, 1);
        assert_eq!(pair.inverse_nonrestoring_count, 1);
        assert_eq!(pair.nonrestoring_inverse_candidate_ids.len(), 1);
        assert!(pair.inverse_matches.is_empty());
        assert!(!pair.eligible);

        let source = periods(" ");
        let forward = observe(&source).unwrap().opportunities.remove(0);
        // Same text, independently supplied parser structure. A parataxis head
        // is absent from the legacy predicate inventory, so the guard rejects.
        let pair = evaluate_counterpart(&source, &forward, &semicolon("he", "parataxis")).unwrap();
        assert_eq!(pair.status, PairStatus::GuardRejected);
        assert_eq!(pair.inverse_matches.len(), 1);
        assert!(!pair.eligible);
        assert!(!pair.inverse_matches[0].eligible);

        // In this annotation the original inverse proposer itself abstains.
        let mut complement = semicolon("he", "conj");
        complement.tokens[1].head = 4;
        complement.tokens[1].dep = "ccomp".into();
        complement.tokens[4].head = 4;
        complement.tokens[4].dep = "ROOT".into();
        complement.sentences[0].root = 4;
        let pair = evaluate_counterpart(&source, &forward, &complement).unwrap();
        assert_eq!(pair.status, PairStatus::NoExactInverse);
        assert_eq!(pair.inverse_proposal_count, 0);
        assert!(!pair.eligible);
    }

    #[test]
    fn mismatched_annotations_and_forged_opportunities_fail_closed() {
        let source = semicolon("he", "conj");
        let opportunity = observe(&source).unwrap().opportunities.remove(0);
        let mut candidate = periods(" ");
        candidate.parser_identity.push('x');
        assert!(evaluate_counterpart(&source, &opportunity, &candidate).is_err());
        assert!(evaluate_counterpart(&source, &opportunity, &periods("\n")).is_err());
        let mut forged = opportunity.clone();
        forged.site_id.push('x');
        assert!(evaluate_counterpart(&source, &forged, &periods(" ")).is_err());
        let mut invalid = source;
        invalid.tokens[0].end_byte -= 1;
        assert!(observe(&invalid).is_err());
    }
}

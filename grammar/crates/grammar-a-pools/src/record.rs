//! The normalised human-pool document, rights gate and date gate.
use crate::text::{self, Excerpt};
use serde_json::{Value, json};

pub const DOCUMENT_SCHEMA: &str = "slop_ninja_a_pool_document_v1";
/// ChatGPT public release; dated human text must precede it.
pub const CUTOFF: &str = "2022-11-30T00:00:00Z";
pub const CUTOFF_DATE: &str = "2022-11-30";

/// FineWeb rows: the ODC-By 1.0 database licence and Common Crawl terms;
/// the page text itself carries no licence grant. Admitted for Train,
/// Train-mining and evaluation only by the user decision of 2026-10-27
/// (crawls before 2022-11-30, ignored storage, never redistributed).
pub const FINEWEB_LICENSE: &str = "ODC-By-1.0-database+CommonCrawl-terms;page-text-unlicensed";

/// PLOS articles under the pre-CC0 "Creative Commons Public Domain
/// declaration", stated in prose without a link (user decision 2026-10-27
/// on prose licence statements; a public-domain dedication).
pub const PUBLIC_DOMAIN_CC_DECLARATION: &str = "public-domain-cc-declaration-prose";

/// Licence or terms ids of sources admitted under the classifier-training
/// policy of 2026-10-27 (lawful access, terms recorded per row,
/// `redistribute_text: false`, removable per-source cohorts). `splits_v4`
/// also refuses every collection its config excludes.
pub const CLASSIFIER_TRAINING_TERMS: &[&str] = &[
    crate::forum::WEBIS_LICENSE,
    crate::forum::IRC_LICENSE,
    "amazon-reviews-2023-citation-requested;review-text-unlicensed",
];
/// The reviews track's name for the same list.
pub const CLASSIFIER_USE_TERMS: &[&str] = CLASSIFIER_TRAINING_TERMS;

/// Licences admitted under the current default decision: public domain,
/// CC0, CC BY and CC BY-SA (any version), plus the listed classifier-training
/// terms. Everything else stays out.
pub fn admitted_license(id: &str) -> bool {
    id == FINEWEB_LICENSE
        || CLASSIFIER_TRAINING_TERMS.contains(&id)
        || crate::pile::TERMS.contains(&id)
        || id == crate::uspto::LICENSE
        || id == crate::btp::LICENSE
        || id == PUBLIC_DOMAIN_CC_DECLARATION
        || id == "public-domain-us"
        || id == "public-domain-us-federal-government"
        || id == "public-domain-us-judicial-opinion"
        || id == "CC0-1.0"
        || id.starts_with("CC-BY-SA-")
        || (id.starts_with("CC-BY-") && !id.starts_with("CC-BY-NC") && !id.starts_with("CC-BY-ND"))
}

/// Basis recorded on rows admitted as classifier-training use (user
/// decision 2026-10-27). Such text is never redistributed, and each source is
/// its own removable cohort. `docs/a-competitive-protocol.md` lists the
/// admitted and the excluded sources.
pub const CLASSIFIER_TRAINING_BASIS: &str = "classifier_training_use_user_decision_2026-10-27";

/// Licence or terms ids admissible only with `CLASSIFIER_TRAINING_BASIS`.
pub const CLASSIFIER_TRAINING_LICENSES: &[&str] = &["CC-BY-ND-3.0-US"];

/// Rights gate on a whole rights object: the default licence set, or a
/// listed classifier-training licence carrying that basis, no redistribution and
/// an explicit split restriction.
pub fn admitted_rights(rights: &Value) -> bool {
    let id = rights["license"].as_str().unwrap_or("");
    admitted_license(id)
        || (CLASSIFIER_TRAINING_LICENSES.contains(&id)
            && rights["admission_basis"] == CLASSIFIER_TRAINING_BASIS
            && rights["redistribute_text"] == false
            && rights["allowed_splits"].as_array().is_some_and(|a| !a.is_empty()))
}

/// Canonical licence id from a Creative Commons URL.
pub fn license_from_url(url: &str) -> Option<String> {
    let u = url.trim().to_lowercase();
    let u = u.trim_end_matches('/');
    if u.contains("creativecommons.org/publicdomain/zero/1.0") {
        return Some("CC0-1.0".into());
    }
    let i = u.find("creativecommons.org/licenses/")?;
    let rest = &u[i + "creativecommons.org/licenses/".len()..];
    let mut parts = rest.split('/');
    let kind = parts.next()?;
    let version = parts.next()?;
    let kind = match kind {
        "by" => "BY",
        "by-sa" => "BY-SA",
        "by-nc" => "BY-NC",
        "by-nd" => "BY-ND",
        "by-nc-sa" => "BY-NC-SA",
        "by-nc-nd" => "BY-NC-ND",
        _ => return None,
    };
    version
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.')
        .then(|| format!("CC-{kind}-{version}"))
}

pub fn license_url(id: &str) -> Option<String> {
    if id == "CC0-1.0" {
        return Some("https://creativecommons.org/publicdomain/zero/1.0/".into());
    }
    let rest = id.strip_prefix("CC-")?;
    let (kind, version) = rest.rsplit_once('-')?;
    if !version.chars().all(|c| c.is_ascii_digit() || c == '.') || version.is_empty() {
        return None;
    }
    Some(format!(
        "https://creativecommons.org/licenses/{}/{}/",
        kind.to_lowercase(),
        version
    ))
}

/// Strictly before the cutoff. Accepts `YYYY-MM-DD...` prefixes.
pub fn before_cutoff(date: &str) -> bool {
    date.len() >= 10 && date.is_char_boundary(10) && valid_date(&date[..10]) && &date[..10] < CUTOFF_DATE
}

pub fn valid_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Everything an adapter knows about one admitted-candidate document.
pub struct Doc<'a> {
    pub id: String,
    pub collection: &'a str,
    pub domain: &'a str,
    pub source_group: String,
    pub writer_key: Option<String>,
    pub evidence: &'a str,
    pub excerpt: &'a Excerpt,
    pub extraction_method: &'a str,
    pub unit: Value,
    pub provenance: Value,
    pub date: Option<String>,
    pub date_basis: &'a str,
    pub cutoff_policy: &'a str,
    pub rights: Value,
    pub aliases: Vec<String>,
}

impl Doc<'_> {
    pub fn to_value(&self) -> Value {
        let t = &self.excerpt.text;
        json!({
            "schema": DOCUMENT_SCHEMA,
            "id": self.id,
            "collection": self.collection,
            "domain": self.domain,
            "source_group": self.source_group,
            "source_group_policy": "All excerpts and later derivatives inherit this source family and its split.",
            "writer_key": self.writer_key,
            "aliases": self.aliases,
            "origin": "human_only",
            "evidence": self.evidence,
            "documented_human_only": false,
            "human_role_present": true,
            "ai_role_present": false,
            "ai_identity": null,
            "share_target": 0.0,
            "share_evidence": {"basis": "historical_human_proxy_endpoint", "not_documented_contemporary_workflow": true},
            "text": t,
            "text_sha256": text::sha_hex(t.as_bytes()),
            "normalized_text_sha256": text::normalized_sha(t),
            "folded_text_sha256": text::folded_sha(t),
            "word_count": text::words(t),
            "word_count_basis": "whitespace_tokens",
            "word_bound": [text::MIN_WORDS, text::MAX_WORDS],
            "word_bound_is_not_tokenizer_qualification": true,
            "extraction": {
                "method": self.extraction_method,
                "unit": self.unit,
                "span": [self.excerpt.start, self.excerpt.end],
                "span_basis": self.unit["span_basis"].as_str().unwrap_or("rendered_paragraph_index_half_open"),
                "complete_unit": self.excerpt.complete_unit,
                "no_truncation_inside_paragraph_or_sentence": self.unit["cut_inside_paragraph_at_sentence_end"] != true,
                "no_prose_repair": true,
            },
            "provenance": self.provenance,
            "date": {"value": self.date, "basis": self.date_basis},
            "cutoff": {"policy": self.cutoff_policy, "before": CUTOFF},
            "rights": self.rights,
            "exposure": {
                "pretraining_exposure": "unknown",
                "grammar_exposure": "none_recorded;fresh_pool_is_not_an_exposure_audit",
                "human_only_production_history": "not_authenticated",
            },
            "pool_status": "candidate",
            "frozen_split": null,
            "split_assigned": false,
        })
    }
}

/// Share-alike and attribution obligations in the established shape.
pub fn rights(license: &str, attribution: String, evidence_url: &str, notes: &str) -> Value {
    let mut v = json!({
        "license": license,
        "license_url": license_url(license),
        "attribution": attribution,
        "evidence_url": evidence_url,
        "commercial_training": true,
        "model_release": true,
        "external_evaluation": true,
        "redistribute_text": true,
        "admission_decision": "default_2026-10-27: public domain, CC0, CC BY and CC BY-SA sources consistent with existing precedent",
        "notes": notes,
    });
    if license.starts_with("CC-BY-SA-") {
        v["share_alike"] = json!({
            "policy": "slop_ninja_wikipedia_share_alike_v1",
            "source_license": license,
            "changes": "Rendered from source markup into plain paragraphs; whole paragraphs selected; no prose rewritten.",
        });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rights_gate_admits_only_the_default_licence_set() {
        for ok in ["public-domain-us", "CC0-1.0", "CC-BY-4.0", "CC-BY-2.5", "CC-BY-SA-3.0"] {
            assert!(admitted_license(ok), "{ok}");
        }
        for no in ["CC-BY-NC-4.0", "CC-BY-ND-4.0", "CC-BY-NC-SA-3.0", "ODC-By-1.0", "unknown"] {
            assert!(!admitted_license(no), "{no}");
        }
        assert_eq!(license_from_url("http://creativecommons.org/licenses/by/4.0/").as_deref(), Some("CC-BY-4.0"));
        assert_eq!(license_from_url("https://creativecommons.org/publicdomain/zero/1.0/").as_deref(), Some("CC0-1.0"));
        assert_eq!(license_from_url("https://creativecommons.org/licenses/by-nc/3.0").as_deref(), Some("CC-BY-NC-3.0"));
        assert_eq!(license_url("CC-BY-SA-2.5").as_deref(), Some("https://creativecommons.org/licenses/by-sa/2.5/"));
        assert!(admitted_license("CC-BY-unversioned-prose-statement"));
        assert_eq!(license_url("CC-BY-unversioned-prose-statement"), None);
        assert!(admitted_license(FINEWEB_LICENSE));
        for t in CLASSIFIER_TRAINING_TERMS {
            assert!(admitted_license(t), "{t}");
        }
        assert!(!admitted_license("yelp-dataset-terms-of-use"));
        assert!(admitted_license(crate::btp::LICENSE));
    }
    #[test]
    fn date_gate_is_strict() {
        assert!(before_cutoff("2022-11-29T23:59:59Z"));
        assert!(!before_cutoff("2022-11-30T00:00:00Z"));
        assert!(!before_cutoff("2023-01-01"));
        assert!(!before_cutoff("unknown"));
    }
}

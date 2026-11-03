//! US Federal Register bulk-XML adapter (govinfo `FR-YYYY-MM-DD.xml`).
//!
//! Rules, proposed rules and notices are works of the US federal
//! government (17 U.S.C. 105). Filings authored by third parties, quoted
//! extracts, tables, regulatory text and footnotes are excluded.
use crate::{
    adapter::Run,
    record::{self, Doc},
    text::{self, Para, Screen},
    xmldoc,
};
use anyhow::Result;
use roxmltree::Node;
use serde_json::json;

const DOC_TYPES: &[&str] = &["RULE", "PRORULE", "NOTICE"];
/// Containers whose content is not the agency's running prose.
const BREAK_CONTAINERS: &[&str] = &[
    "GPOTABLE", "FTNT", "EXTRACT", "GPH", "MATH", "LSTSUB", "REGTEXT", "AMDPAR", "SIG", "BILCOD",
    "STARS", "NOTE", "AUTH", "CONTENTS", "SECTION", "APPENDIX", "PRTPAGE",
];
const GAP_ELEMENTS: &[&str] = &["SU", "FTREF", "GPH", "MATH", "GPOTABLE"];
/// Page markers are layout, not text.
const DROP_ELEMENTS: &[&str] = &["PRTPAGE"];
const THIRD_PARTY_SUBJECTS: &[&str] = &["self-regulatory organization", "self regulatory organization"];
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// `FR-2019-01-02.xml` -> `2019-01-02`.
pub fn date_from_name(n: &str) -> Option<String> {
    let d = n.strip_prefix("FR-")?.get(..10)?;
    record::valid_date(d).then(|| d.to_owned())
}

/// `[FR Doc. 2019-00001 Filed 1-2-19; 8:45 am]` -> `2019-00001`.
///
/// Strict shape: an optional capital letter, digits, then one or two
/// `-digits` groups (`00-1234`, `E8-7811`, `R1-2009-1234`). The number must
/// end at whitespace, `]`, `;`, the end, or a glued `Filed`. Anything else
/// (`00-`, `1313`, `05-10263-`, `00-1234s`) is malformed and yields `None`.
pub fn frdoc_id(s: &str) -> Option<String> {
    let i = s.find("FR Doc.")?;
    let t = s[i + 7..].trim_start();
    let b = t.as_bytes();
    let mut k = 0;
    if b.first().is_some_and(|c| c.is_ascii_uppercase()) {
        k = 1;
    }
    let digits = |k: &mut usize| {
        let s = *k;
        while b.get(*k).is_some_and(|c| c.is_ascii_digit()) {
            *k += 1;
        }
        *k > s
    };
    if !digits(&mut k) {
        return None;
    }
    let mut groups = 0;
    while groups < 2 && b.get(k) == Some(&b'-') && b.get(k + 1).is_some_and(|c| c.is_ascii_digit()) {
        k += 1;
        digits(&mut k);
        groups += 1;
    }
    if groups == 0 {
        return None;
    }
    let rest = &t[k..];
    let ends = rest.is_empty()
        || rest.starts_with(|c: char| c.is_whitespace() || c == ']' || c == ';')
        || rest.starts_with("Filed");
    ends.then(|| t[..k].to_owned())
}

/// FR document numbers in a whole input, with multiplicity (ids only; no
/// prose is extracted). A number printed on two documents is ambiguous: the
/// source record cannot say which document the number and URL belong to.
fn frdoc_counts(source: &xmldoc::Source) -> Result<std::collections::HashMap<String, u32>> {
    let mut counts = std::collections::HashMap::new();
    source.each(MAX_FILE_BYTES, |name, bytes| {
        if date_from_name(name).is_none() {
            return Ok(true);
        }
        let Some(bytes) = bytes else { return Ok(true) };
        let Ok(xml) = std::str::from_utf8(bytes) else { return Ok(true) };
        let prepared = xmldoc::prepare(xml);
        let Ok(doc) = xmldoc::document(&prepared) else { return Ok(true) };
        for d in doc.descendants().filter(|n| DOC_TYPES.contains(&n.tag_name().name())) {
            if let Some(id) = d.descendants().find(|n| n.has_tag_name("FRDOC")).and_then(|n| frdoc_id(&xmldoc::text_of(n))) {
                *counts.entry(id).or_insert(0u32) += 1;
            }
        }
        Ok(true)
    })?;
    Ok(counts)
}

fn paragraphs(n: Node, out: &mut Vec<Para>) {
    for c in n.children().filter(|c| c.is_element()) {
        let name = c.tag_name().name();
        if name == "P" || name == "FP" {
            out.push(xmldoc::paragraph(c, GAP_ELEMENTS, DROP_ELEMENTS, Screen::XML));
        } else if name == "HD" {
            out.push(Para::non_prose(xmldoc::text_of(c), "heading"));
        } else if BREAK_CONTAINERS.contains(&name) {
            out.push(Para::non_prose(String::new(), "excluded_container"));
        } else {
            paragraphs(c, out);
        }
    }
}

pub fn adapt(source: &xmldoc::Source, corpus: &str, run: &mut Run) -> Result<()> {
    let counts = frdoc_counts(source)?;
    source.each(MAX_FILE_BYTES, |name, bytes| {
        if run.full() {
            return Ok(false);
        }
        issue(name, bytes, corpus, &counts, run)?;
        Ok(!run.full())
    })
}

fn issue(name: &str, bytes: Option<&[u8]>, corpus: &str, counts: &std::collections::HashMap<String, u32>, run: &mut Run) -> Result<()> {
    let method = format!("federal_register_SUM_then_SUPLINF_P_elements;{}", text::PARAGRAPH_RUN_METHOD);
    {
        let file_unit = format!("fr_file:{name}");
        let Some(issue) = date_from_name(name) else {
            return run.reject(&file_unit, "file_name_without_issue_date");
        };
        let Some(bytes) = bytes else {
            return run.reject(&file_unit, "file_unreadable");
        };
        let Ok(xml) = std::str::from_utf8(bytes) else {
            return run.reject(&file_unit, "not_utf8");
        };
        let prepared = xmldoc::prepare(xml);
        let Ok(doc) = xmldoc::document(&prepared) else {
            return run.reject(&file_unit, "xml_unparsed");
        };
        let file_sha = text::sha_hex(bytes);
        for d in doc.descendants().filter(|n| DOC_TYPES.contains(&n.tag_name().name())) {
            if run.full() {
                break;
            }
            run.units_seen += 1;
            let Some(frdoc_el) = d.descendants().find(|n| n.has_tag_name("FRDOC")) else {
                run.reject(&format!("{file_unit}:{}", d.range().start), "no_fr_doc_number")?;
                continue;
            };
            let Some(frdoc) = frdoc_id(&xmldoc::text_of(frdoc_el)) else {
                run.reject(&format!("{file_unit}:{}", d.range().start), "malformed_fr_doc_number")?;
                continue;
            };
            let unit = format!("federal_register:{frdoc}");
            let meta = || {
                let first = |tag: &str| d.descendants().find(|n| n.has_tag_name(tag)).map(xmldoc::text_of);
                let agency = first("AGENCY").unwrap_or_default();
                let subject = first("SUBJECT").unwrap_or_default();
                json!({"fr_doc": frdoc, "agency": agency, "writer_key": (!agency.is_empty()).then(|| format!("federal_register_agency:{}", text::underscore_key(&agency))),
                       "subagencies": d.descendants().filter(|n| n.has_tag_name("SUBAGY")).map(xmldoc::text_of).collect::<Vec<_>>(),
                       "document_type": d.tag_name().name(), "issue_date": issue, "file_name": name, "file_sha256": file_sha,
                       "subject_sha256": text::sha_hex(subject.as_bytes()),
                       "third_party_subject": THIRD_PARTY_SUBJECTS.iter().any(|t| subject.to_lowercase().contains(t)),
                       "fr_doc_number_count_in_input": counts.get(&frdoc).copied().unwrap_or(0), "corpus": corpus})
            };
            if !run.sampled_meta(&unit, meta)? {
                continue;
            }
            if counts.get(&frdoc).copied().unwrap_or(0) > 1 {
                run.reject(&format!("{unit}@{file_unit}:{}", d.range().start), "fr_doc_number_repeated_in_input")?;
                continue;
            }
            let first = |tag: &str| d.descendants().find(|n| n.has_tag_name(tag)).map(xmldoc::text_of);
            let agency = first("AGENCY").unwrap_or_default();
            let subject = first("SUBJECT").unwrap_or_default();
            let all = xmldoc::text_of(d).to_lowercase();
            if !record::before_cutoff(&issue) {
                run.reject(&unit, "issue_not_before_2022_11_30")?;
                continue;
            }
            if agency.is_empty() {
                run.reject(&unit, "no_agency")?;
                continue;
            }
            if all.contains("copyright") || all.contains("copyrighted") {
                run.reject(&unit, "copyright_mention_requires_review")?;
                continue;
            }
            if THIRD_PARTY_SUBJECTS.iter().any(|t| subject.to_lowercase().contains(t)) {
                run.reject(&unit, "third_party_authored_filing")?;
                continue;
            }
            let mut paras = Vec::new();
            for part in ["SUM", "SUPLINF"] {
                if let Some(p) = d.descendants().find(|n| n.has_tag_name(part)) {
                    paras.push(Para::non_prose(String::new(), "part_boundary"));
                    paragraphs(p, &mut paras);
                }
            }
            let Some(ex) = text::paragraph_run(&paras) else {
                run.reject(&unit, "no_paragraph_run_within_bounds")?;
                continue;
            };
            let agency_key = text::underscore_key(&agency);
            let url = format!("https://www.federalregister.gov/d/{frdoc}");
            let doc = Doc {
                id: format!("pool-fr-{frdoc}"),
                collection: "us_federal_register_historical",
                domain: "government",
                source_group: unit.clone(),
                writer_key: Some(format!("federal_register_agency:{agency_key}")),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind":format!("federal_register_{}", d.tag_name().name().to_lowercase()),
                             "file_sha256":file_sha,"file_name":name}),
                provenance: json!({"source_url":url,"fr_doc":frdoc,"agency":agency,"subject":subject,
                                   "document_type":d.tag_name().name(),"issue_date":issue,"corpus":corpus}),
                date: Some(issue.clone()),
                date_basis: "federal_register_issue_date",
                cutoff_policy: "dated_before_cutoff",
                rights: record::rights(
                    "public-domain-us-federal-government",
                    format!("Federal Register, {agency}, {subject}, FR Doc. {frdoc}, {issue}; {url}"),
                    "https://www.govinfo.gov/help/fr",
                    "Work of the US federal government (17 U.S.C. 105); institutional authorship, individual identity unavailable. Documents mentioning copyright and self-regulatory-organisation filings are excluded.",
                ),
                aliases: vec![],
            };
            run.accept(&unit, &doc)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn notice(id: &str, agency: &str, subject: &str, paras: &[String]) -> String {
        let ps: String = paras.iter().map(|p| format!("<P>{p}</P>")).collect();
        format!("<NOTICE><PREAMB><AGENCY TYPE=\"N\">{agency}</AGENCY><SUBJECT>{subject}</SUBJECT><SUM><HD SOURCE=\"HED\">SUMMARY:</HD><P>Short summary of the notice here for readers.</P></SUM></PREAMB><SUPLINF><HD SOURCE=\"HED\">SUPPLEMENTARY INFORMATION:</HD><HD SOURCE=\"HD1\">I. Background</HD>{ps}<EXTRACT><P>Quoted material that is not ours at all and should never count.</P></EXTRACT></SUPLINF><FRDOC>[FR Doc. {id} Filed 1-2-19; 8:45 am]</FRDOC></NOTICE>")
    }

    #[test]
    fn notices_extract_agency_prose_only() {
        let s = |n: usize| format!("{} under the program<SU>1</SU>.", vec!["The agency reviewed"; n].join(" "));
        let ok = |n: usize| format!("{} under the program today.", vec!["The agency reviewed"; n].join(" "));
        let xml = format!("<?xml version=\"1.0\"?><FEDREG><NOTICES>{}{}{}</NOTICES></FEDREG>",
            notice("2019-00001", "Department of Synthetic Affairs", "Program review", &[ok(40), ok(40)]),
            notice("2019-00002", "Securities and Exchange Commission", "Self-Regulatory Organizations; Proposed Rule Change", &[ok(40), ok(40)]),
            notice("2019-00003", "Department of Synthetic Affairs", "Footnoted", &[s(80)]));
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("FR-2019-01-02.xml");
        std::fs::write(&f, xml).unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "federal-register", "us_federal_register_historical", json!({}), None).unwrap();
        adapt(&xmldoc::Source::Files(vec![f]), "synthetic", &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["rejection_reasons"]["third_party_authored_filing"], 1);
        assert_eq!(rep["rejection_reasons"]["no_paragraph_run_within_bounds"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["source_group"], "federal_register:2019-00001");
        assert_eq!(rows[0]["writer_key"], "federal_register_agency:department_of_synthetic_affairs");
        assert_eq!(rows[0]["word_count"], 248);
        assert!(!rows[0]["text"].as_str().unwrap().contains("Quoted"));
        assert_eq!(frdoc_id("[FR Doc. 2020-12345 Filed 6-1-20; 8:45 am]").as_deref(), Some("2020-12345"));
    }

    #[test]
    fn doc_numbers_are_strict_and_repeats_are_rejected() {
        let f = |s: &str| frdoc_id(&format!("[FR Doc. {s} Filed 1-2-00; 8:45 am]"));
        assert_eq!(f("E8-7811").as_deref(), Some("E8-7811"));
        assert_eq!(f("R1-2009-1234").as_deref(), Some("R1-2009-1234"));
        assert_eq!(frdoc_id("[FR Doc. 06-8484Filed 1-2-06]").as_deref(), Some("06-8484"));
        assert_eq!(frdoc_id("[FR Doc. 00-1234]").as_deref(), Some("00-1234"));
        for bad in ["00-", "1313", "05-10263-", "00-1234s", "-12"] {
            assert_eq!(f(bad), None, "{bad}");
        }
        let ok = |n: usize| format!("{} under the program today.", vec!["The agency reviewed"; n].join(" "));
        let day = |ids: &[&str]| {
            let ns: String = ids.iter().map(|i| notice(i, "Department of Synthetic Affairs", "Program review", &[ok(40), ok(40)])).collect();
            format!("<?xml version=\"1.0\"?><FEDREG><NOTICES>{ns}</NOTICES></FEDREG>")
        };
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("FR-2019-01-02.xml");
        let b = d.path().join("FR-2019-01-03.xml");
        std::fs::write(&a, day(&["2019-00001", "2019-00002", "2019-0000x"])).unwrap();
        std::fs::write(&b, day(&["2019-00002", "2019-00003"])).unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "federal-register", "us_federal_register_historical", json!({}), None).unwrap();
        adapt(&xmldoc::Source::Files(vec![a, b]), "synthetic", &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 2);
        assert_eq!(rep["rejection_reasons"]["fr_doc_number_repeated_in_input"], 2);
        assert_eq!(rep["rejection_reasons"]["malformed_fr_doc_number"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["pool-fr-2019-00001", "pool-fr-2019-00003"]);
    }
}

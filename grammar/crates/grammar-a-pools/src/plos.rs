//! PLOS JATS adapter (for example the `allofplos` corpus): main abstract
//! paragraphs, optionally the introduction, under an explicit CC link.
use crate::{
    adapter::Run,
    record::{self, Doc},
    text::{self, Para, Screen},
    xmldoc,
};
use anyhow::Result;
use roxmltree::Node;
use serde_json::json;

const XLINK: &str = "http://www.w3.org/1999/xlink";
const GAP_ELEMENTS: &[&str] = &[
    "xref", "inline-formula", "disp-formula", "math", "fig", "table-wrap", "inline-graphic",
    "graphic", "sup", "sub", "fn", "supplementary-material", "media", "list", "disp-quote",
];
const MAX_ARTICLE_BYTES: u64 = 32 * 1024 * 1024;

fn child<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.has_tag_name(name))
}

pub struct Meta {
    pub doi: String,
    pub title: String,
    pub journal: String,
    pub article_type: String,
    pub authors: Vec<String>,
    pub date: Option<String>,
    pub license_urls: Vec<String>,
    /// Text of the `<license>` elements (`license-p`), whitespace-squashed.
    pub license_text: String,
}

pub fn meta(doc: &roxmltree::Document) -> Option<Meta> {
    let article = doc.root_element();
    let front = child(article, "front")?;
    let am = child(front, "article-meta")?;
    let doi = am
        .children()
        .find(|c| c.has_tag_name("article-id") && c.attribute("pub-id-type") == Some("doi"))
        .map(xmldoc::text_of)?;
    let title = am.descendants().find(|c| c.has_tag_name("article-title")).map(xmldoc::text_of).unwrap_or_default();
    let journal = child(front, "journal-meta")
        .and_then(|j| j.descendants().find(|c| c.has_tag_name("journal-title")))
        .map(xmldoc::text_of)
        .unwrap_or_default();
    let mut authors = Vec::new();
    for c in am.descendants().filter(|c| c.has_tag_name("contrib") && c.attribute("contrib-type") == Some("author")) {
        if let Some(n) = child(c, "name") {
            let given = child(n, "given-names").map(xmldoc::text_of).unwrap_or_default();
            let sur = child(n, "surname").map(xmldoc::text_of).unwrap_or_default();
            authors.push(format!("{given} {sur}").trim().to_owned());
        } else if let Some(col) = child(c, "collab") {
            authors.push(xmldoc::text_of(col));
        }
    }
    let pub_date = am
        .children()
        .filter(|c| c.has_tag_name("pub-date"))
        .find(|c| c.attribute("pub-type") == Some("epub") || c.attribute("publication-format") == Some("electronic"))
        .or_else(|| am.children().find(|c| c.has_tag_name("pub-date")));
    let date = pub_date.and_then(|p| {
        let num = |k: &str| child(p, k).and_then(|x| xmldoc::text_of(x).parse::<u32>().ok());
        Some(format!("{:04}-{:02}-{:02}", num("year")?, num("month")?, num("day")?))
    });
    let mut license_urls = Vec::new();
    let mut license_text = Vec::new();
    for l in am.descendants().filter(|c| c.has_tag_name("license")) {
        license_text.push(xmldoc::text_of(l));
        if let Some(h) = l.attribute((XLINK, "href")) {
            license_urls.push(h.to_owned());
        }
        for r in l.descendants().filter(|c| c.tag_name().name() == "license_ref") {
            license_urls.push(xmldoc::text_of(r));
        }
    }
    Some(Meta {
        doi: doi.to_lowercase(),
        title,
        journal,
        article_type: article.attribute("article-type").unwrap_or("").to_owned(),
        authors,
        date,
        license_urls,
        license_text: text::squash(&license_text.join(" ")),
    })
}

/// One admitted licence from explicit CC links, if they agree.
pub fn license(urls: &[String]) -> Result<String, &'static str> {
    let mut ids: Vec<String> = urls.iter().filter_map(|u| record::license_from_url(u)).collect();
    ids.sort();
    ids.dedup();
    match ids.as_slice() {
        [] => Err("no_explicit_creative_commons_link"),
        [one] if record::admitted_license(one) => Ok(one.clone()),
        [_] => Err("license_not_admitted"),
        _ => Err("conflicting_license_links"),
    }
}

pub const PROSE_CC_BY: &str = "CC-BY-unversioned-prose-statement";
pub const PLOS_LICENSE_POLICY_URL: &str = "https://plos.org/terms-of-use/";

/// Licence from a prose statement without a Creative Commons link: CC BY
/// ("Creative Commons Attribution License") unless it names a
/// non-commercial or no-derivatives term; CC0 for the public-domain
/// dedication; the pre-CC0 "Creative Commons Public Domain declaration" as
/// its own public-domain id. PLOS's early own licence and the UK Open
/// Government Licence are not Creative Commons statements and stay out. Admitted by the user decision of 2026-10-27 (PLOS publishes
/// all content under CC BY or CC0 by policy); the version is not stated.
pub fn prose_license(t: &str) -> Result<String, &'static str> {
    let l = t.to_lowercase();
    if l.is_empty() {
        return Err("no_licence_statement");
    }
    if ["noncommercial", "non-commercial", "no derivative", "noderiv", "non commercial"].iter().any(|x| l.contains(x)) {
        return Err("prose_licence_not_admitted");
    }
    if l.contains("cc0") || l.contains("public domain dedication") {
        return Ok("CC0-1.0".into());
    }
    if l.contains("creative commons public domain declaration") {
        return Ok(record::PUBLIC_DOMAIN_CC_DECLARATION.into());
    }
    if l.contains("creative commons attribution") {
        return Ok(PROSE_CC_BY.into());
    }
    Err("prose_licence_not_recognized")
}

fn abstract_paras(doc: &roxmltree::Document) -> Vec<Para> {
    let Some(abs) = doc
        .descendants()
        .find(|n| n.has_tag_name("abstract") && n.attribute("abstract-type").is_none())
    else {
        return Vec::new();
    };
    abs.descendants()
        .filter(|n| n.has_tag_name("p"))
        .map(|p| xmldoc::paragraph(p, GAP_ELEMENTS, &[], Screen::XML))
        .collect()
}

fn introduction_paras(doc: &roxmltree::Document) -> Vec<Para> {
    let Some(body) = doc.descendants().find(|n| n.has_tag_name("body")) else {
        return Vec::new();
    };
    let intro = body.children().filter(|c| c.has_tag_name("sec")).find(|s| {
        s.attribute("sec-type") == Some("intro")
            || child(*s, "title").is_some_and(|t| xmldoc::text_of(t).to_lowercase().starts_with("introduction"))
    });
    let Some(intro) = intro else { return Vec::new() };
    // Direct paragraphs only; subsections and their titles break the run.
    intro
        .children()
        .filter(|c| c.is_element())
        .map(|c| {
            if c.has_tag_name("p") {
                xmldoc::paragraph(c, GAP_ELEMENTS, &[], Screen::XML)
            } else {
                Para::non_prose(String::new(), "non_paragraph_element")
            }
        })
        .collect()
}

pub struct Options<'a> {
    pub corpus: &'a str,
    pub corpus_sha256: Option<&'a str>,
    pub include_introduction: bool,
    /// Only articles without a Creative Commons link whose prose licence
    /// statement is CC BY or CC0 (`prose_license`); link-licensed articles
    /// are counted and skipped (they belong to the explicit-link route).
    pub prose_licence_only: bool,
}

pub fn adapt(source: &xmldoc::Source, opt: &Options, run: &mut Run) -> Result<()> {
    source.each(MAX_ARTICLE_BYTES, |name, bytes| {
        if run.full() {
            return Ok(false);
        }
        article(name, bytes, opt, run)?;
        Ok(true)
    })
}

fn article(name: &str, bytes: Option<&[u8]>, opt: &Options, run: &mut Run) -> Result<()> {
    let method_abs = format!("jats_main_abstract_p_elements_section_titles_omitted;{}", text::PARAGRAPH_RUN_METHOD);
    let method_intro = format!("jats_introduction_direct_p_elements;{}", text::PARAGRAPH_RUN_METHOD);
    run.units_seen += 1;
    let unit = format!("plos_file:{name}");
    let Some(bytes) = bytes else {
        return run.reject(&unit, "file_unreadable");
    };
    let Ok(xml) = std::str::from_utf8(bytes) else {
        return run.reject(&unit, "not_utf8");
    };
    let prepared = xmldoc::prepare(xml);
    let Ok(doc) = xmldoc::document(&prepared) else {
        return run.reject(&unit, "xml_unparsed");
    };
    let Some(m) = meta(&doc) else {
        return run.reject(&unit, "no_doi_metadata");
    };
    let unit = format!("doi:{}", m.doi);
    let meta = || {
        json!({"doi": m.doi, "file_name": name, "file_sha256": text::sha_hex(bytes), "journal": m.journal,
               "article_type": m.article_type, "authors": m.authors, "published": m.date,
               "title_sha256": text::sha_hex(m.title.as_bytes()), "license_urls": m.license_urls,
               "license": license(&m.license_urls).ok(), "plos_doi": m.doi.starts_with("10.1371/")})
    };
    if !run.sampled_meta(&unit, meta)? {
        return Ok(());
    }
    if !m.doi.starts_with("10.1371/") {
        return run.reject(&unit, "not_plos_doi");
    }
    let (lic, licence_basis) = match (license(&m.license_urls), opt.prose_licence_only) {
        (Ok(l), false) => (l, "explicit_creative_commons_link"),
        (Ok(_), true) => {
            run.count("explicit_link_licence_route_not_this_run");
            return Ok(());
        }
        (Err("no_explicit_creative_commons_link"), true) => match prose_license(&m.license_text) {
            Ok(l) => (l, "prose_statement_without_link"),
            Err(r) => return run.reject(&unit, r),
        },
        (Err(r), _) => {
            return run.reject(&unit, r);
        }
    };
    let Some(date) = m.date.clone() else {
        return run.reject(&unit, "no_publication_date");
    };
    if !record::before_cutoff(&date) {
        return run.reject(&unit, "published_not_before_2022_11_30");
    }
    let file_sha = text::sha_hex(bytes);
    let mut units = vec![("abstract", abstract_paras(&doc), &method_abs)];
    if opt.include_introduction {
        units.push(("introduction", introduction_paras(&doc), &method_intro));
    }
    for (kind, paras, method) in units {
        let id = format!("pool-plos-{}-{kind}", m.doi.replace('/', "_"));
        let Some(ex) = text::paragraph_run(&paras) else {
            run.reject(&id, "no_paragraph_run_within_bounds")?;
            continue;
        };
        let url = format!("https://doi.org/{}", m.doi);
        let lic_url = record::license_url(&lic).unwrap_or_else(|| PLOS_LICENSE_POLICY_URL.to_owned());
        let doc = Doc {
            id: id.clone(),
            collection: "plos_historical",
            domain: "science",
            source_group: unit.clone(),
            writer_key: None,
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: method,
            unit: json!({"kind":format!("jats_{kind}"),"file_sha256":file_sha,"file_name":name}),
            provenance: json!({"source_url":url,"doi":m.doi,"title":m.title,"journal":m.journal,
                               "article_type":m.article_type,"authors":m.authors,"published":date,
                               "license_links":m.license_urls,"corpus":opt.corpus,"corpus_sha256":opt.corpus_sha256}),
            date: Some(date.clone()),
            date_basis: "jats_epub_date",
            cutoff_policy: "dated_before_cutoff",
            rights: {
                let mut r = record::rights(
                    &lic,
                    format!("{}; {}; {url}; {lic_url}; {kind} paragraphs extracted, markup and section titles omitted", m.authors.join(", "), m.title),
                    &url,
                    "Coauthored scientific writing; individual human identity unavailable.",
                );
                if licence_basis == "prose_statement_without_link" {
                    r["licence_basis"] = json!(licence_basis);
                    r["licence_statement_sha256"] = json!(text::sha_hex(m.license_text.as_bytes()));
                    r["license_url"] = json!(PLOS_LICENSE_POLICY_URL);
                    r["admission_decision"] = json!("user_2026-10-27: PLOS articles whose CC BY or CC0 statement is prose without a link are admitted (PLOS publishes all content under CC BY or CC0 by policy); Train and Train-mining only");
                    r["licence_version_stated"] = json!(false);
                }
                r
            },
            aliases: vec![],
        };
        run.accept(&id, &doc)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn zip_archives_are_read_in_name_order() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("allofplos.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
        let abs = format!("<p>{} end.</p>", vec!["We found"; 100].join(" "));
        for (n, doi) in [("b/journal.pone.2.xml", "10.1371/journal.pone.2"), ("a/journal.pone.1.xml", "10.1371/journal.pone.1")] {
            w.start_file(n, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(article(doi, "http://creativecommons.org/licenses/by/4.0/", 2012, &abs).as_bytes()).unwrap();
        }
        w.start_file("README.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.finish().unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "plos", "plos_historical", json!({}), None).unwrap();
        let src = xmldoc::Source::open(&z).unwrap();
        adapt(&src, &Options { corpus: "synthetic", corpus_sha256: None, include_introduction: true, prose_licence_only: false }, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["units_seen"], 2);
        assert_eq!(rep["candidates"], 2);
        assert_eq!(rep["rejection_reasons"]["no_paragraph_run_within_bounds"], 2);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["provenance"]["doi"], "10.1371/journal.pone.1");
        assert_eq!(rows[0]["extraction"]["unit"]["file_name"], "journal.pone.1.xml");
    }

    pub fn article(doi: &str, lic: &str, year: u32, abs: &str) -> String {
        format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE article PUBLIC "-//NLM//DTD JATS (Z39.96) Journal Publishing DTD v1.1d3 20150301//EN" "http://jats.nlm.nih.gov/publishing/1.1d3/JATS-journalpublishing1.dtd">
<article xmlns:xlink="http://www.w3.org/1999/xlink" xmlns:mml="http://www.w3.org/1998/Math/MathML" article-type="research-article">
<front><journal-meta><journal-title-group><journal-title>PLOS ONE</journal-title></journal-title-group></journal-meta>
<article-meta><article-id pub-id-type="doi">{doi}</article-id>
<title-group><article-title>A synthetic study</article-title></title-group>
<contrib-group><contrib contrib-type="author"><name><surname>Roe</surname><given-names>Jane</given-names></name></contrib></contrib-group>
<pub-date pub-type="epub"><day>15</day><month>1</month><year>{year}</year></pub-date>
<permissions><license xlink:href="{lic}"><license-p>Open access.</license-p></license></permissions>
<abstract>{abs}</abstract></article-meta></front>
<body><sec sec-type="intro"><title>Introduction</title><p>Intro text.</p></sec></body></article>"#)
    }

    #[test]
    fn abstract_extraction_with_licence_and_date_gates() {
        let p = |n: usize| format!("<p>{} result&nbsp;here.</p>", vec!["We measured"; n].join(" "));
        let abs = format!("<sec><title>Background</title>{}</sec><sec><title>Methods</title>{}</sec>", p(60), p(60));
        let d = tempfile::tempdir().unwrap();
        let files = [
            ("a.xml", article("10.1371/journal.pone.0000001", "http://creativecommons.org/licenses/by/4.0/", 2019, &abs)),
            ("b.xml", article("10.1371/journal.pone.0000002", "https://creativecommons.org/licenses/by-nc/4.0/", 2019, &abs)),
            ("c.xml", article("10.1371/journal.pone.0000003", "http://creativecommons.org/licenses/by/4.0/", 2023, &abs)),
            ("d.xml", article("10.1371/journal.pone.0000004", "https://creativecommons.org/publicdomain/zero/1.0/", 2015, &format!("<p>Ratio <inline-formula>x</inline-formula> {} end.</p>", vec!["w"; 200].join(" ")))),
        ];
        let mut paths = Vec::new();
        for (n, x) in &files {
            let path = d.path().join(n);
            std::fs::write(&path, x).unwrap();
            paths.push(path);
        }
        let out = d.path().join("out");
        let mut run = Run::start(&out, "plos", "plos_historical", json!({}), None).unwrap();
        adapt(&xmldoc::Source::Files(paths), &Options { corpus: "synthetic", corpus_sha256: None, include_introduction: false, prose_licence_only: false }, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["rejection_reasons"]["license_not_admitted"], 1);
        assert_eq!(rep["rejection_reasons"]["published_not_before_2022_11_30"], 1);
        assert_eq!(rep["rejection_reasons"]["no_paragraph_run_within_bounds"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["source_group"], "doi:10.1371/journal.pone.0000001");
        assert_eq!(rows[0]["rights"]["license"], "CC-BY-4.0");
        assert_eq!(rows[0]["word_count"], 244);
        assert!(!rows[0]["text"].as_str().unwrap().contains("Background"));
    }

    #[test]
    fn prose_licence_route_admits_only_unlinked_cc_by_and_cc0() {
        let p = |n: usize| format!("<p>{} result here.</p>", vec!["We measured"; n].join(" "));
        let abs = p(120);
        let prose = |doi: &str, stmt: &str| {
            article(doi, "http://creativecommons.org/licenses/by/4.0/", 2009, &abs)
                .replace(r#"<license xlink:href="http://creativecommons.org/licenses/by/4.0/"><license-p>Open access.</license-p></license>"#, &format!("<license><license-p>{stmt}</license-p></license>"))
        };
        let d = tempfile::tempdir().unwrap();
        let files = [
            ("a.xml", prose("10.1371/journal.pone.0000011", "This is an open-access article distributed under the terms of the Creative Commons Attribution License, which permits unrestricted use.")),
            ("b.xml", prose("10.1371/journal.pone.0000012", "This is an open-access article, free of all copyright, and may be freely reproduced; made available under the Creative Commons CC0 public domain dedication.")),
            ("c.xml", prose("10.1371/journal.pone.0000013", "Distributed under the Creative Commons Attribution-NonCommercial License.")),
            ("e.xml", prose("10.1371/journal.pone.0000015", "This is an open-access article distributed under the terms of the Creative Commons Public Domain declaration which stipulates that the work may be freely reproduced.")),
            ("f.xml", prose("10.1371/journal.pone.0000016", "This is an open-access article distributed under the terms of the Public Library of Science Open-Access License.")),
            ("d.xml", article("10.1371/journal.pone.0000014", "http://creativecommons.org/licenses/by/4.0/", 2019, &abs)),
        ];
        let mut paths = Vec::new();
        for (n, x) in &files {
            let path = d.path().join(n);
            std::fs::write(&path, x).unwrap();
            paths.push(path);
        }
        let out = d.path().join("out");
        let mut run = Run::start(&out, "plos", "plos_historical", json!({}), None).unwrap();
        adapt(&xmldoc::Source::Files(paths), &Options { corpus: "synthetic", corpus_sha256: None, include_introduction: false, prose_licence_only: true }, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 3);
        assert_eq!(rep["rejection_reasons"]["prose_licence_not_admitted"], 1);
        assert_eq!(rep["rejection_reasons"]["prose_licence_not_recognized"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["explicit_link_licence_route_not_this_run"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["rights"]["license"], PROSE_CC_BY);
        assert_eq!(rows[0]["rights"]["licence_basis"], "prose_statement_without_link");
        assert_eq!(rows[1]["rights"]["license"], "CC0-1.0");
        assert_eq!(rows[0]["rights"]["license_url"], PLOS_LICENSE_POLICY_URL);
        assert_eq!(rows[2]["rights"]["license"], record::PUBLIC_DOMAIN_CC_DECLARATION);
    }
}

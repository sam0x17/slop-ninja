//! Streaming MediaWiki XML export reader and the Wikipedia and Wikinews
//! adapters built on it.
use crate::{
    adapter::Run,
    record::{self, Doc},
    text::{self, Screen},
    wikitext,
};
use anyhow::{Context, Result, bail};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
use std::io::BufRead;

#[derive(Debug, Default, Clone)]
pub struct Page {
    pub title: String,
    pub ns: String,
    pub id: String,
    pub redirect: bool,
    pub revision_id: String,
    pub parent_id: String,
    pub timestamp: String,
    pub sha1: String,
    pub model: String,
    pub text: String,
}

/// Call `f` for every `<page>` of a pages-articles export.
pub fn pages<R: BufRead>(r: R, mut f: impl FnMut(Page) -> Result<bool>) -> Result<()> {
    let mut reader = Reader::from_reader(r);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut page = Page::default();
    let mut field: Option<String> = None;
    loop {
        match reader.read_event_into(&mut buf).context("MediaWiki XML")? {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_vec();
                if name == b"page" {
                    page = Page::default();
                }
                path.push(name);
                field = Some(String::new());
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"redirect" && path.last().is_some_and(|p| p == b"page") {
                    page.redirect = true;
                }
            }
            Event::Text(t) => {
                if let Some(f) = field.as_mut() {
                    // References arrive as separate GeneralRef events.
                    f.push_str(&t.decode()?);
                }
            }
            Event::CData(t) => {
                if let Some(f) = field.as_mut() {
                    f.push_str(&t.decode()?);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(f) = field.as_mut() {
                    match r.resolve_char_ref()? {
                        Some(c) => f.push(c),
                        None => match r.decode()?.as_ref() {
                            "amp" => f.push('&'),
                            "lt" => f.push('<'),
                            "gt" => f.push('>'),
                            "quot" => f.push('"'),
                            "apos" => f.push('\''),
                            other => bail!("undeclared XML entity {other}"),
                        },
                    }
                }
            }
            Event::End(_) => {
                let name = path.pop().unwrap_or_default();
                let parent = path.last().map(Vec::as_slice);
                let value = field.take().unwrap_or_default();
                match (parent, name.as_slice()) {
                    (Some(b"page"), b"title") => page.title = value,
                    (Some(b"page"), b"ns") => page.ns = value,
                    (Some(b"page"), b"id") => page.id = value,
                    (Some(b"revision"), b"id") => page.revision_id = value,
                    (Some(b"revision"), b"parentid") => page.parent_id = value,
                    (Some(b"revision"), b"timestamp") => page.timestamp = value,
                    (Some(b"revision"), b"sha1") => page.sha1 = value,
                    (Some(b"revision"), b"model") => page.model = value,
                    (Some(b"revision"), b"text") => page.text = value,
                    (_, b"page") => {
                        if !f(std::mem::take(&mut page))? {
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                // Text belongs to the innermost open element only.
                field = path.last().map(|_| String::new()).filter(|_| false);
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
        buf.clear();
    }
}

/// Identifiers and hashes of one page for metadata-only reservation; the
/// title is kept only as a hash and the wikitext is never rendered.
fn page_meta(p: &Page) -> Value {
    json!({"page_id": p.id, "ns": p.ns, "redirect": p.redirect || p.text.trim_start().to_lowercase().starts_with("#redirect"),
           "revision_id": p.revision_id, "parent_revision_id": p.parent_id, "timestamp": p.timestamp,
           "revision_sha1_base36": p.sha1, "revision_text_sha256": text::sha_hex(p.text.as_bytes()), "model": p.model,
           "title_sha256": text::sha_hex(p.title.as_bytes())})
}

/// Precedent terms that send a page to separate rights review.
const WIKIPEDIA_NOTICE_TERMS: &[&str] = &[
    "copyvio", "copyright", "fair use", "fairuse", "cc-notice", "cc-by", "attribution", "copied",
    "public domain", "pd-notice", "eb1911", "gfdl", "plagiar", "permission=",
];
const TRANSLATION_TERMS: &[&str] = &[
    "{{translated", "{{rough translation", "{{machine translation", "{{expand language",
    "{{translation", "{{ai-generated", "{{llm",
];
const DISAMBIGUATION: &[&str] = &[
    "{{disambig", "{{dab", "{{set index", "{{hndis", "{{geodis", "disambiguation}}", "{{surname", "{{given name",
];

pub fn wikipedia_page_screen(p: &Page) -> Option<&'static str> {
    if p.ns != "0" {
        return Some("not_article_namespace");
    }
    if p.redirect || p.text.trim_start().to_lowercase().starts_with("#redirect") {
        return Some("redirect");
    }
    if !p.model.is_empty() && p.model != "wikitext" {
        return Some("not_wikitext");
    }
    if !record::before_cutoff(&p.timestamp) {
        return Some("revision_not_before_2022_11_30");
    }
    if p.title.starts_with("List of") || p.title.starts_with("Lists of") || p.title.contains("(disambiguation)") {
        return Some("list_or_disambiguation_title");
    }
    let l = p.text.to_lowercase();
    if DISAMBIGUATION.iter().any(|t| l.contains(t)) {
        return Some("disambiguation_or_name_list");
    }
    if TRANSLATION_TERMS.iter().any(|t| l.contains(t)) {
        return Some("translation_or_generated_text_notice");
    }
    if WIKIPEDIA_NOTICE_TERMS.iter().any(|t| l.contains(t)) {
        return Some("source_notice_requires_separate_review");
    }
    None
}

/// Wikitext before the first section heading line. Rendering only the lead
/// keeps a full-dump pass linear; a heading inside an unclosed template
/// leaves it unbalanced, which can only reject the page.
pub fn lead_wikitext(t: &str) -> &str {
    let mut pos = 0;
    for line in t.split_inclusive('\n') {
        let l = line.trim();
        if l.len() >= 2 && l.starts_with("==") && l.ends_with('=') {
            return &t[..pos];
        }
        pos += line.len();
    }
    t
}

pub struct DumpInfo {
    pub name: String,
    pub sha256: Option<String>,
    pub url: Option<String>,
}

pub fn wikipedia<R: BufRead>(input: R, dump: &DumpInfo, run: &mut Run) -> Result<()> {
    let method = format!("{};lead_section;{}", wikitext::RENDER_METHOD, text::PARAGRAPH_RUN_METHOD);
    pages(input, |p| {
        run.units_seen += 1;
        let unit = format!("wikipedia:page:{}", p.id);
        if !run.sampled_meta(&unit, || page_meta(&p))? {
            return Ok(true);
        }
        if let Some(r) = wikipedia_page_screen(&p) {
            if matches!(r, "not_article_namespace" | "redirect") {
                run.count(r);
            } else {
                run.reject(&unit, r)?;
            }
            return Ok(true);
        }
        // A renderer defect on one page rejects that page instead of
        // aborting a multi-hour dump pass; the reason stays in the ledger.
        let rendered = std::panic::catch_unwind(|| {
            let sections = wikitext::render(lead_wikitext(&p.text), Screen::MARKUP);
            text::paragraph_run(&sections[0].paras)
        });
        let Ok(rendered) = rendered else {
            run.reject(&unit, "renderer_panic")?;
            return Ok(true);
        };
        let Some(ex) = rendered else {
            run.reject(&unit, "no_lead_paragraph_run_within_bounds")?;
            return Ok(true);
        };
        let rev_url = format!("https://en.wikipedia.org/w/index.php?oldid={}", p.revision_id);
        let history = format!("https://en.wikipedia.org/w/index.php?curid={}&action=history", p.id);
        let doc = Doc {
            id: format!("pool-wikipedia-{}-r{}-lead", p.id, p.revision_id),
            collection: "wikipedia_enwiki_historical",
            domain: "encyclopedia",
            source_group: unit.clone(),
            writer_key: None,
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind":"mediawiki_revision","section":"lead","revision_sha1_base36":p.sha1,
                         "revision_text_sha256":text::sha_hex(p.text.as_bytes())}),
            provenance: json!({"source_url":rev_url,"history_url":history,"dump":dump.name,"dump_url":dump.url,
                               "dump_sha256":dump.sha256,"page_id":p.id,"revision_id":p.revision_id,
                               "parent_revision_id":p.parent_id,"title":p.title}),
            date: Some(p.timestamp.clone()),
            date_basis: "latest_revision_timestamp_in_dump",
            cutoff_policy: "dated_before_cutoff",
            rights: record::rights(
                "CC-BY-SA-3.0",
                format!("Wikipedia contributors, {}, {rev_url}; history {history}; original text CC BY-SA 3.0 Unported, https://creativecommons.org/licenses/by-sa/3.0/ .", p.title),
                "https://en.wikipedia.org/wiki/Wikipedia:Copyrights",
                "Collaborative editing; individual human identity unavailable. Pre-cutoff revisions are licensed CC BY-SA 3.0 (and GFDL).",
            ),
            aliases: vec![format!("wikipedia_title:{}", p.title)],
        };
        run.accept(&unit, &doc)?;
        Ok(!run.full())
    })
}

/// Wikinews relicensed to CC BY 2.5 on 2005-09-25.
pub const WIKINEWS_CC_BY_FROM: &str = "2005-09-25";
const WIKINEWS_PUBLISHED: &[&str] = &["{{publish", "{{archive", "{{archived"];
const WIKINEWS_UNPUBLISHED: &[&str] = &[
    "{{develop", "{{review", "{{tasks", "{{abandoned", "{{delete", "{{prod", "{{disputed",
    "{{copyvio", "{{breaking review", "{{under review", "{{not ready", "{{minimal",
];
const WIKINEWS_NOTICE_TERMS: &[&str] = &[
    "copyvio", "fair use", "fairuse", "plagiar", "pd-notice", "cc-notice", "eb1911",
];
const WIKINEWS_STOP_HEADINGS: &[&str] = &[
    "sources", "source", "related news", "related articles", "external links", "references",
    "see also", "sister links", "notes",
];

const MONTHS: [&str; 12] = [
    "january", "february", "march", "april", "may", "june", "july", "august", "september",
    "october", "november", "december",
];

/// The `{{date|...}}` publication date as `YYYY-MM-DD`.
pub fn wikinews_date(wikitext: &str) -> Option<String> {
    let l = wikitext.to_lowercase();
    let i = l.find("{{date|")?;
    let inner = &l[i + 7..];
    let inner = &inner[..inner.find("}}")?];
    let inner = inner.trim();
    if record::valid_date(inner) {
        return Some(inner.to_owned());
    }
    // "May 5, 2008" or "5 May 2008"
    let toks: Vec<&str> = inner.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    let (mut y, mut m, mut d) = (None, None, None);
    for t in toks {
        if let Some(k) = MONTHS.iter().position(|mm| *mm == t) {
            m = Some(k + 1);
        } else if let Ok(n) = t.parse::<u32>() {
            if n > 31 {
                y = Some(n);
            } else {
                d = Some(n);
            }
        }
    }
    Some(format!("{:04}-{:02}-{:02}", y?, m?, d?))
}

pub fn wikinews_page_screen(p: &Page) -> Option<&'static str> {
    if p.ns != "0" {
        return Some("not_article_namespace");
    }
    if p.redirect || p.text.trim_start().to_lowercase().starts_with("#redirect") {
        return Some("redirect");
    }
    if !p.model.is_empty() && p.model != "wikitext" {
        return Some("not_wikitext");
    }
    if !record::before_cutoff(&p.timestamp) {
        return Some("revision_not_before_2022_11_30");
    }
    let l = p.text.to_lowercase();
    if !WIKINEWS_PUBLISHED.iter().any(|t| l.contains(t)) {
        return Some("not_published");
    }
    if WIKINEWS_UNPUBLISHED.iter().any(|t| l.contains(t)) {
        return Some("unpublished_or_disputed_status");
    }
    if TRANSLATION_TERMS.iter().any(|t| l.contains(t)) {
        return Some("translation_or_generated_text_notice");
    }
    if WIKINEWS_NOTICE_TERMS.iter().any(|t| l.contains(t)) {
        return Some("source_notice_requires_separate_review");
    }
    None
}

pub fn wikinews<R: BufRead>(input: R, dump: &DumpInfo, run: &mut Run) -> Result<()> {
    let method = format!("{};body_before_sources;{}", wikitext::RENDER_METHOD, text::PARAGRAPH_RUN_METHOD);
    pages(input, |p| {
        run.units_seen += 1;
        let unit = format!("wikinews:{}", p.id);
        if !run.sampled_meta(&unit, || page_meta(&p))? {
            return Ok(true);
        }
        if let Some(r) = wikinews_page_screen(&p) {
            if matches!(r, "not_article_namespace" | "redirect") {
                run.count(r);
            } else {
                run.reject(&unit, r)?;
            }
            return Ok(true);
        }
        let Some(published) = wikinews_date(&p.text) else {
            run.reject(&unit, "no_publication_date_template")?;
            return Ok(true);
        };
        if published.as_str() < WIKINEWS_CC_BY_FROM {
            run.reject(&unit, "published_before_cc_by_2_5_relicensing")?;
            return Ok(true);
        }
        if !record::before_cutoff(&published) {
            run.reject(&unit, "published_not_before_2022_11_30")?;
            return Ok(true);
        }
        let mut paras = Vec::new();
        for s in wikitext::render(&p.text, Screen::MARKUP) {
            if s.heading.as_deref().is_some_and(|h| WIKINEWS_STOP_HEADINGS.contains(&h.to_lowercase().as_str())) {
                break;
            }
            if s.heading.is_some() {
                paras.push(text::Para::non_prose(String::new(), "heading"));
            }
            paras.extend(s.paras);
        }
        let Some(ex) = text::paragraph_run(&paras) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            return Ok(true);
        };
        let rev_url = format!("https://en.wikinews.org/w/index.php?oldid={}", p.revision_id);
        let history = format!("https://en.wikinews.org/w/index.php?curid={}&action=history", p.id);
        let doc = Doc {
            id: format!("pool-wikinews-{}-r{}", p.id, p.revision_id),
            collection: "wikinews_enwikinews_historical",
            domain: "news",
            source_group: unit.clone(),
            writer_key: None,
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind":"mediawiki_revision","section":"body_before_sources","revision_sha1_base36":p.sha1,
                         "revision_text_sha256":text::sha_hex(p.text.as_bytes())}),
            provenance: json!({"source_url":rev_url,"history_url":history,"dump":dump.name,"dump_url":dump.url,
                               "dump_sha256":dump.sha256,"page_id":p.id,"revision_id":p.revision_id,
                               "parent_revision_id":p.parent_id,"title":p.title,"published":published,
                               "latest_revision_timestamp":p.timestamp}),
            date: Some(published.clone()),
            date_basis: "wikinews_date_template_and_latest_revision_before_cutoff",
            cutoff_policy: "dated_before_cutoff",
            rights: record::rights(
                "CC-BY-2.5",
                format!("Wikinews; {}; {rev_url}; CC BY 2.5; contributor history {history}", p.title),
                "https://en.wikinews.org/wiki/Wikinews:Copyright",
                "Collaborative reporting; individual human identity unavailable. Quotations of eight or more words break paragraph runs.",
            ),
            aliases: vec![format!("wikinews_title:{}", p.title)],
        };
        run.accept(&unit, &doc)?;
        Ok(!run.full())
    })
}

pub fn dump_value(d: &DumpInfo) -> Value {
    json!({"dump":d.name,"dump_url":d.url,"declared_dump_sha256":d.sha256})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml(pages: &str) -> String {
        format!("<mediawiki xmlns=\"http://www.mediawiki.org/xml/export-0.10/\"><siteinfo><sitename>Synthetic</sitename></siteinfo>{pages}</mediawiki>")
    }
    fn page(id: u32, title: &str, ts: &str, body: &str) -> String {
        format!("<page><title>{title}</title><ns>0</ns><id>{id}</id><revision><id>{}</id><parentid>1</parentid><timestamp>{ts}</timestamp><contributor><username>x</username><id>9</id></contributor><model>wikitext</model><format>text/x-wiki</format><text bytes=\"1\" xml:space=\"preserve\">{body}</text><sha1>abc</sha1></revision></page>", id * 10)
    }
    fn sentence(n: usize) -> String {
        format!("{} end.", vec!["word"; n - 1].join(" "))
    }

    #[test]
    fn stream_reads_fields_and_entities() {
        let x = xml(&page(7, "A &amp; B", "2020-01-01T00:00:00Z", "Text &lt;b&gt; &#233;"));
        let mut got = Vec::new();
        pages(x.as_bytes(), |p| {
            got.push(p);
            Ok(true)
        })
        .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].title.as_str(), got[0].id.as_str(), got[0].revision_id.as_str()), ("A & B", "7", "70"));
        assert_eq!(got[0].text, "Text <b> é");
        assert_eq!(got[0].sha1, "abc");
    }

    #[test]
    fn wikipedia_adapter_screens_and_extracts_lead() {
        let lead = format!("'''Synthville''' {}\n\n{}\n\n== History ==\n{}", sentence(120), sentence(110), sentence(200));
        let x = xml(&[
            page(1, "Synthville", "2021-05-01T00:00:00Z", &lead),
            page(2, "Later", "2023-01-01T00:00:00Z", &lead),
            page(3, "Notice", "2021-05-01T00:00:00Z", &format!("{lead} public domain")),
            page(4, "List of things", "2021-05-01T00:00:00Z", &lead),
        ].concat());
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("w");
        let mut run = Run::start(&out, "wikipedia", "wikipedia_enwiki_historical", json!({}), None).unwrap();
        let info = DumpInfo { name: "synthetic".into(), sha256: None, url: None };
        wikipedia(x.as_bytes(), &info, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["rejection_reasons"]["revision_not_before_2022_11_30"], 1);
        assert_eq!(rep["rejection_reasons"]["source_notice_requires_separate_review"], 1);
        assert_eq!(rep["rejection_reasons"]["list_or_disambiguation_title"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["source_group"], "wikipedia:page:1");
        assert_eq!(rows[0]["word_count"], 231);
        assert_eq!(rows[0]["rights"]["license"], "CC-BY-SA-3.0");
        assert!(rows[0]["text"].as_str().unwrap().starts_with("Synthville word"));
    }

    #[test]
    fn short_lead_with_unclosed_tag_prefix_does_not_panic() {
        // Regression: a needle longer than the remaining text panicked in find_ci.
        // Bodies are XML-escaped; they decode to "<", "<r", "x<ref", "<ref" and so on.
        for body in ["", "&lt;", "&lt;r", "x&lt;ref", "&lt;ref", "a\n&lt;ref name=x"] {
            let x = xml(&page(1, "Short", "2021-05-01T00:00:00Z", body));
            let d = tempfile::tempdir().unwrap();
            let mut run = Run::start(&d.path().join("w"), "wikipedia", "wikipedia_enwiki_historical", json!({}), None).unwrap();
            let info = DumpInfo { name: "synthetic".into(), sha256: None, url: None };
            wikipedia(x.as_bytes(), &info, &mut run).unwrap();
            let rep = run.finish(json!({})).unwrap();
            assert_eq!(rep["candidates"], 0);
            assert!(rep["rejection_reasons"].get("renderer_panic").is_none(), "{body:?}");
        }
    }

    #[test]
    fn lead_stops_at_first_heading_and_sampling_is_by_id() {
        assert_eq!(lead_wikitext("Lead text.\n\n== History ==\nMore."), "Lead text.\n\n");
        assert_eq!(lead_wikitext("No headings at all."), "No headings at all.");
        let d = tempfile::tempdir().unwrap();
        let mut run = Run::start_sampled(&d.path().join("s"), "wikipedia", "x", json!({}), None, Some((0, 500))).unwrap();
        let kept = (0..2000).filter(|i| run.sampled(&format!("wikipedia:page:{i}"))).count();
        assert!((850..1150).contains(&kept), "{kept}");
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["counted_without_ledger_rows"]["family_bucket_outside_run_range"], 2000 - kept as u64);
        assert_eq!(rep["family_buckets"], json!([0, 500]));
        // Complementary ranges partition families exactly.
        let mut other = Run::start_sampled(&d.path().join("t"), "wikipedia", "x", json!({}), None, Some((500, 1000))).unwrap();
        let rest = (0..2000).filter(|i| other.sampled(&format!("wikipedia:page:{i}"))).count();
        assert_eq!(kept + rest, 2000);
        assert!(crate::adapter::parse_buckets("10..5").is_err());
        assert_eq!(crate::adapter::parse_buckets("0..800").unwrap(), (0, 800));
    }

    #[test]
    fn wikinews_requires_publication_and_licence_date() {
        assert_eq!(wikinews_date("{{date|May 5, 2008}}").as_deref(), Some("2008-05-05"));
        assert_eq!(wikinews_date("{{Date|2011-02-03}}").as_deref(), Some("2011-02-03"));
        let body = |date: &str, status: &str| format!("{{{{date|{date}}}}}\n{}\n\n{}\n== Sources ==\n{}\n{status}", sentence(130), sentence(100), sentence(100));
        let x = xml(&[
            page(1, "Ok", "2012-01-01T00:00:00Z", &body("January 2, 2012", "{{publish}}")),
            page(2, "Draft", "2012-01-01T00:00:00Z", &body("January 2, 2012", "{{develop}}")),
            page(3, "Old", "2005-01-01T00:00:00Z", &body("January 2, 2005", "{{publish}}")),
        ].concat());
        let d = tempfile::tempdir().unwrap();
        let mut run = Run::start(&d.path().join("n"), "wikinews", "wikinews_enwikinews_historical", json!({}), None).unwrap();
        let info = DumpInfo { name: "synthetic".into(), sha256: None, url: None };
        wikinews(x.as_bytes(), &info, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["rejection_reasons"]["not_published"], 1);
        assert_eq!(rep["rejection_reasons"]["published_before_cc_by_2_5_relicensing"], 1);
    }
}

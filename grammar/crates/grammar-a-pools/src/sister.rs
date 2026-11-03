//! Wikimedia sister projects from pre-cutoff pages-articles dumps: English
//! Wikivoyage (travel guides), Wikibooks (textbooks, plus the Cookbook
//! namespace as its own collection) and Wikisource (transcribed historical
//! texts). Rendering, the paragraph-run rule and the dump reader are the
//! Wikipedia and Wikinews ones.
//!
//! Grouping. The source family is the conflict unit at dedup and the bucket
//! key (`writer_key`) is the split group:
//! - Wikivoyage: family and group are the page.
//! - Wikibooks: family is the page; group is the book (title before the
//!   first `/`), so one book never spans splits. Cookbook recipes are their
//!   own group.
//! - Wikisource: family is the work (main-namespace root title, or the scan
//!   index of a `Page:` transcription); group is the author when the work
//!   metadata names one, else the work. The author group key is
//!   `author:<first_last name key>`, the Project Gutenberg writer key, so a
//!   writer reserved for Gutenberg evaluation (writer bucket 900..1000) is
//!   never rendered by a Train-side Wikisource run either.
//!
//! Each group keeps at most `max_per_group` documents, chosen by the lowest
//! `h64("slop_ninja_sister_cap_v1\0" + unit id)`, never by text or score.
//!
//! Wikisource needs work metadata (header year, author, translator,
//! licence templates, scan-index fields) that lives on other pages, so it is
//! two passes: `wikisource_meta` writes text-free metadata rows, and the
//! adapter reads them back.
use crate::{
    adapter::Run,
    io::JsonlWriter,
    mediawiki::{self, DumpInfo, Page},
    record::{self, Doc},
    text::{self, Excerpt, Para, Screen},
    wikitext,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    io::BufRead,
    path::Path,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Project {
    Wikivoyage,
    Wikibooks,
    Wikisource,
}

impl std::str::FromStr for Project {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "wikivoyage" => Project::Wikivoyage,
            "wikibooks" => Project::Wikibooks,
            "wikisource" => Project::Wikisource,
            _ => anyhow::bail!("project must be wikivoyage, wikibooks or wikisource"),
        })
    }
}

pub const WIKIVOYAGE: &str = "wikivoyage_enwikivoyage_historical";
pub const WIKIBOOKS: &str = "wikibooks_enwikibooks_historical";
pub const COOKBOOK: &str = "wikibooks_cookbook_historical";
pub const WIKISOURCE: &str = "wikisource_enwikisource_historical";
pub const CAP_DOMAIN: &str = "slop_ninja_sister_cap_v1\0";

impl Project {
    pub fn adapter(self) -> &'static str {
        match self {
            Project::Wikivoyage => "wikivoyage",
            Project::Wikibooks => "wikibooks",
            Project::Wikisource => "wikisource",
        }
    }
    /// Main collection (the run's report collection).
    pub fn collection(self) -> &'static str {
        match self {
            Project::Wikivoyage => WIKIVOYAGE,
            Project::Wikibooks => WIKIBOOKS,
            Project::Wikisource => WIKISOURCE,
        }
    }
    fn host(self) -> &'static str {
        match self {
            Project::Wikivoyage => "en.wikivoyage.org",
            Project::Wikibooks => "en.wikibooks.org",
            Project::Wikisource => "en.wikisource.org",
        }
    }
    fn copyright_page(self) -> &'static str {
        match self {
            Project::Wikivoyage => "https://en.wikivoyage.org/wiki/Wikivoyage:Copyleft",
            Project::Wikibooks => "https://en.wikibooks.org/wiki/Wikibooks:Copyrights",
            Project::Wikisource => "https://en.wikisource.org/wiki/Wikisource:Copyright_policy",
        }
    }
    pub fn default_cap(self) -> usize {
        match self {
            Project::Wikivoyage => 1,
            Project::Wikibooks => 10,
            Project::Wikisource => 5,
        }
    }
}

const STOP_HEADINGS: &[&str] = &[
    "references", "external links", "see also", "notes", "sources", "further reading", "bibliography",
    "footnotes", "citations", "go next", "stay safe", "connect", "licensing", "license",
];
const TRANSLATION_TERMS: &[&str] = &[
    "{{translated", "{{rough translation", "{{machine translation", "{{translation", "{{ai-generated", "{{llm",
    "{{transwiki", "{{imported", "{{wikitravel",
];
const NOTICE_TERMS: &[&str] = &[
    "{{copyvio", "{{copyright", "{{fair use", "{{fairuse", "{{non-free", "{{nonfree", "{{plagiar", "{{delete",
    "{{speedy", "{{rfd", "{{no license", "{{nolicense", "{{no source", "{{nosource", "eb1911", "{{copied",
];
const DISAMBIGUATION: &[&str] = &[
    "{{disamb", "{{dab", "{{disambiguation", "{{versions", "{{translations", "{{similar", "{{author disambiguation",
    "{{set index", "{{surname",
];
const PRINT_VERSION_SUFFIXES: &[&str] = &["/print version", "/printable version", "/all chapters", "/print", "/printable"];

/// Top-level templates of `s` as (lowercased name, inner text).
pub fn templates(s: &str) -> Vec<(String, &str)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if &b[i..i + 2] != b"{{" {
            i += 1;
            continue;
        }
        let mut depth = 0usize;
        let mut j = i;
        let mut end = None;
        while j + 1 < b.len() {
            if &b[j..j + 2] == b"{{" {
                depth += 1;
                j += 2;
            } else if &b[j..j + 2] == b"}}" {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    end = Some(j);
                    break;
                }
            } else {
                j += 1;
            }
        }
        let Some(end) = end else { break };
        let inner = &s[i + 2..end - 2];
        out.push((template_name(inner), inner));
        i = end;
    }
    out
}

fn template_name(inner: &str) -> String {
    let n = inner.split(['|', '\n']).next().unwrap_or("").trim().trim_start_matches(':').replace('_', " ").to_lowercase();
    n.trim_start_matches("template:").trim().to_owned()
}

/// Named parameter `key` (case-insensitive) of a template's inner text,
/// split on `|` outside nested templates and links.
pub fn param(inner: &str, key: &str) -> Option<String> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let b = inner.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if i + 1 < b.len() && (&b[i..i + 2] == b"{{" || &b[i..i + 2] == b"[[") {
            depth += 1;
            i += 2;
            continue;
        }
        if i + 1 < b.len() && (&b[i..i + 2] == b"}}" || &b[i..i + 2] == b"]]") {
            depth -= 1;
            i += 2;
            continue;
        }
        if b[i] == b'|' && depth == 0 {
            parts.push(&inner[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    parts.push(&inner[start..]);
    parts.iter().skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k.trim().eq_ignore_ascii_case(key)).then(|| v.trim().to_owned())
    })
}

/// First plausible four-digit year (1000..=2022) in a metadata value.
pub fn year_of(v: &str) -> Option<u32> {
    let b = v.as_bytes();
    (0..b.len().saturating_sub(3)).find_map(|i| {
        let w = &b[i..i + 4];
        let edge = (i == 0 || !b[i - 1].is_ascii_digit()) && (i + 4 == b.len() || !b[i + 4].is_ascii_digit());
        if !edge || !w.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let y: u32 = std::str::from_utf8(w).ok()?.parse().ok()?;
        (1000..=2022).contains(&y).then_some(y)
    })
}

/// Name from an author field: link target without `Author:`, else the text.
pub fn author_name(v: &str) -> Option<String> {
    let v = v.trim();
    let name = if let Some(i) = v.find("[[") {
        let rest = &v[i + 2..];
        let inner = &rest[..rest.find("]]").unwrap_or(rest.len())];
        let target = inner.split('|').next().unwrap_or("");
        let t = target.trim().trim_start_matches(':');
        let t = t.strip_prefix("Author:").or_else(|| t.strip_prefix("author:")).unwrap_or(t);
        t.to_owned()
    } else {
        v.to_owned()
    };
    let key = text::folded_tokens(&name).join("_");
    let generic = ["", "anonymous", "unknown", "various", "anon", "not_mentioned", "none"];
    (!generic.contains(&key.as_str())).then_some(key)
}

/// Licence id from template names: PD (US federal government separately),
/// CC BY-SA, CC BY, CC0. Non-commercial or no-derivatives templates never
/// qualify.
pub fn licence_from_templates<'a>(names: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<String> = None;
    for n in names {
        let n = n.replace(' ', "-");
        let id = if n.starts_with("pd-usgov") || n.starts_with("pd/usgov") {
            Some("public-domain-us-federal-government".to_owned())
        } else if n.starts_with("pd-") || n.starts_with("pd/") || n == "pd" {
            Some("public-domain-us".to_owned())
        } else if n == "cc0" || n.starts_with("cc-zero") || n.starts_with("cc0-") {
            Some("CC0-1.0".to_owned())
        } else if n.starts_with("cc-by") && !n.contains("-nc") && !n.contains("-nd") {
            let sa = n.starts_with("cc-by-sa");
            let ver = n.rsplit('-').next().filter(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.') && v.contains('.'));
            Some(match (sa, ver) {
                (true, Some(v)) => format!("CC-BY-SA-{v}"),
                (true, None) => "CC-BY-SA-unversioned-template".to_owned(),
                (false, Some(v)) => format!("CC-BY-{v}"),
                (false, None) => "CC-BY-unversioned-template".to_owned(),
            })
        } else {
            None
        };
        if id.is_some() && best.is_none() {
            best = id;
        }
    }
    best
}

/// Proofread level and body of a `Page:` transcription: the text between
/// the header `</noinclude>` and the footer `<noinclude>`.
pub fn page_body(t: &str) -> Option<(u8, &str)> {
    let q = t.find("<pagequality")?;
    let lv = t[q..].find("level=")? + q + 6;
    let level = t[lv..].trim_start_matches(['"', '\'']).chars().next()?.to_digit(10)? as u8;
    let start = t.find("</noinclude>")? + "</noinclude>".len();
    let end = t.rfind("<noinclude>").filter(|&e| e >= start).unwrap_or(t.len());
    Some((level, &t[start..end]))
}

/// `Page:Name.djvu/12` -> `Name.djvu`.
pub fn page_index(title: &str) -> Option<&str> {
    let rest = title.strip_prefix("Page:")?;
    let (idx, n) = rest.rsplit_once('/')?;
    (!idx.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty()).then_some(idx)
}

fn root_title(title: &str) -> &str {
    title.split('/').next().unwrap_or(title)
}

fn key(s: &str) -> String {
    text::folded_tokens(s).join("_")
}

/// Indexes named by `<pages index="X" ...>` in main-namespace wikitext.
pub fn transcluded_indexes(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(i) = rest.find("<pages") {
        rest = &rest[i + 6..];
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        if let Some(j) = tag.find("index=") {
            let v = tag[j + 6..].trim_start();
            let (q, v) = match v.chars().next() {
                Some(c @ ('"' | '\'')) => (Some(c), &v[1..]),
                _ => (None, v),
            };
            let end = match q {
                Some(c) => v.find(c),
                None => v.find([' ', '/', '>']),
            }
            .unwrap_or(v.len());
            out.push(v[..end].trim().replace('_', " "));
        }
    }
    out
}

/// One text-free Wikisource metadata row (first pass). Titles and
/// bibliographic fields only; no prose.
fn meta_row(p: &Page) -> Option<Value> {
    let ts = templates(&p.text);
    match p.ns.as_str() {
        "0" => {
            let header = ts.iter().find(|(n, _)| n == "header" || n.starts_with("header2"));
            let get = |k: &str| header.and_then(|(_, h)| param(h, k));
            let licence = licence_from_templates(ts.iter().map(|(n, _)| n.as_str()));
            let lower = p.text.to_lowercase();
            Some(json!({"kind": "work_page", "page_id": p.id, "title": p.title, "root": root_title(&p.title),
                        "year": get("year").as_deref().and_then(year_of), "author": get("author").as_deref().and_then(author_name),
                        "translator": get("translator").filter(|t| !t.trim().is_empty() && author_name(t).is_some()).map(|t| key(&t)),
                        "licence": licence, "indexes": transcluded_indexes(&p.text),
                        "disambiguation": DISAMBIGUATION.iter().any(|d| lower.contains(d)),
                        "redirect": p.redirect || lower.trim_start().starts_with("#redirect")}))
        }
        "106" => {
            let (_, inner) = ts.iter().find(|(n, _)| n.contains("proofreadpage index template"))?;
            let get = |k: &str| param(inner, k);
            let index = p.title.strip_prefix("Index:")?.to_owned();
            Some(json!({"kind": "index", "page_id": p.id, "index": index, "year": get("Year").as_deref().and_then(year_of),
                        "author": get("Author").as_deref().and_then(author_name),
                        "translator": get("Translator").filter(|t| !t.trim().is_empty() && author_name(t).is_some()).map(|t| key(&t)),
                        "language": get("Language").map(|l| l.trim().to_lowercase()), "progress": get("Progress"),
                        "type": get("Type")}))
        }
        _ => None,
    }
}

/// First Wikisource pass: metadata rows for main-namespace pages and scan
/// indexes. Returns counts; the caller writes `meta-report.json`.
pub fn wikisource_meta<R: BufRead>(input: R, out: &Path) -> Result<Value> {
    crate::io::fresh_dir(out)?;
    let mut w = JsonlWriter::create(&out.join("meta.jsonl"))?;
    let (mut pages, mut works, mut indexes) = (0u64, 0u64, 0u64);
    mediawiki::pages(input, |p| {
        pages += 1;
        if let Some(v) = meta_row(&p) {
            if v["kind"] == "index" {
                indexes += 1;
            } else {
                works += 1;
            }
            w.write(&v)?;
        }
        Ok(true)
    })?;
    let (sha, rows) = w.finish()?;
    let v = json!({"schema": "slop_ninja_a_pool_wikisource_meta_v1", "pages_seen": pages, "work_pages": works, "indexes": indexes,
                   "rows": rows, "meta_sha256": sha, "prose": false});
    Ok(v)
}

#[derive(Default, Clone)]
pub struct WorkMeta {
    pub year: Option<u32>,
    pub author: Option<String>,
    pub translator: bool,
    pub licence: Option<String>,
    pub disambiguation: bool,
}

#[derive(Default, Clone)]
pub struct IndexMeta {
    pub year: Option<u32>,
    pub author: Option<String>,
    pub translator: bool,
    pub language: Option<String>,
    /// Licence of a main-namespace page that transcludes this index.
    pub licence: Option<String>,
}

#[derive(Default)]
pub struct Meta {
    pub works: HashMap<String, WorkMeta>,
    pub indexes: HashMap<String, IndexMeta>,
}

impl Meta {
    pub fn load(path: &Path) -> Result<Self> {
        let mut m = Meta::default();
        let mut links: Vec<(String, Option<String>)> = Vec::new();
        for r in crate::io::jsonl_rows(path)? {
            let r = r?;
            let s = |k: &str| r[k].as_str().map(str::to_owned);
            let year = r["year"].as_u64().map(|y| y as u32);
            if r["kind"] == "index" {
                let idx = s("index").unwrap_or_default();
                m.indexes.insert(idx, IndexMeta { year, author: s("author"), translator: r["translator"].is_string(), language: s("language"), licence: None });
            } else {
                let lic = s("licence");
                for i in r["indexes"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    links.push((i.to_owned(), lic.clone()));
                }
                let w = WorkMeta { year, author: s("author"), translator: r["translator"].is_string(), licence: lic, disambiguation: r["disambiguation"] == true };
                m.works.insert(s("title").unwrap_or_default(), w);
            }
        }
        for (i, lic) in links {
            if let (Some(x), Some(l)) = (m.indexes.get_mut(&i), lic) {
                x.licence.get_or_insert(l);
            }
        }
        Ok(m)
    }
}

/// A candidate held until its group's cap is resolved.
struct Held {
    rank: u64,
    unit: String,
    id: String,
    collection: &'static str,
    domain: &'static str,
    source_group: String,
    group: String,
    excerpt: Excerpt,
    method: String,
    unit_v: Value,
    provenance: Value,
    date: String,
    date_basis: &'static str,
    rights: Value,
    aliases: Vec<String>,
}

fn general_screen(p: &Page) -> Option<&'static str> {
    if p.redirect || p.text.trim_start().to_lowercase().starts_with("#redirect") {
        return Some("redirect");
    }
    if !p.model.is_empty() && p.model != "wikitext" && p.model != "proofread-page" {
        return Some("not_wikitext");
    }
    if !record::before_cutoff(&p.timestamp) {
        return Some("revision_not_before_2022_11_30");
    }
    let l = p.text.to_lowercase();
    if TRANSLATION_TERMS.iter().any(|t| l.contains(t)) {
        return Some("translation_import_or_generated_text_notice");
    }
    if NOTICE_TERMS.iter().any(|t| l.contains(t)) {
        return Some("source_notice_requires_separate_review");
    }
    if DISAMBIGUATION.iter().any(|t| l.contains(t)) {
        return Some("disambiguation_or_versions_page");
    }
    None
}

/// Paragraphs of a whole page: headings break runs; stop headings end it.
fn page_paras(t: &str) -> Vec<Para> {
    let mut paras = Vec::new();
    for s in wikitext::render(t, Screen::MARKUP) {
        if s.heading.as_deref().is_some_and(|h| STOP_HEADINGS.contains(&h.to_lowercase().trim())) {
            break;
        }
        if s.heading.is_some() {
            paras.push(Para::non_prose(String::new(), "heading"));
        }
        paras.extend(s.paras);
    }
    paras
}

/// Excerpt-level screens shared by the three projects.
fn excerpt_screen(ex: &Excerpt) -> Option<&'static str> {
    if let Some(r) = text::english_screen(&ex.text) {
        return Some(r);
    }
    if let Some(r) = text::strong_transcription_artifact(&ex.text) {
        return Some(r);
    }
    text::markup_residue(&ex.text).into_iter().next()
}

pub struct Options<'a> {
    pub project: Project,
    pub dump: &'a DumpInfo,
    pub max_per_group: usize,
    pub meta: Option<&'a Meta>,
}

pub fn adapt<R: BufRead>(input: R, o: &Options, run: &mut Run) -> Result<Value> {
    let method = format!("{};whole_page_headings_break;{}", wikitext::RENDER_METHOD, text::PARAGRAPH_RUN_METHOD);
    let page_method = format!("{};wikisource_page_body_first_and_last_paragraph_dropped;{}", wikitext::RENDER_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut held: BTreeMap<String, Vec<Held>> = BTreeMap::new();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut weak: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_ns: BTreeMap<String, u64> = BTreeMap::new();
    let host = o.project.host();
    mediawiki::pages(input, |p| {
        run.units_seen += 1;
        let ns_ok = match o.project {
            Project::Wikivoyage => p.ns == "0",
            Project::Wikibooks => p.ns == "0" || p.ns == "102",
            Project::Wikisource => p.ns == "0" || p.ns == "104",
        };
        if !ns_ok {
            run.count("namespace_not_in_scope");
            return Ok(true);
        }
        // Family, split group and collection from identifiers only.
        let (family, group, collection, domain) = match o.project {
            Project::Wikivoyage => {
                let f = format!("wikivoyage:page:{}", p.id);
                (f.clone(), f, WIKIVOYAGE, "travel_guide")
            }
            Project::Wikibooks if p.ns == "102" => {
                (format!("wikibooks:page:{}", p.id), format!("wikibooks:cookbook_page:{}", p.id), COOKBOOK, "recipe")
            }
            Project::Wikibooks => (format!("wikibooks:page:{}", p.id), format!("wikibooks:book:{}", key(root_title(&p.title))), WIKIBOOKS, "textbook"),
            Project::Wikisource => {
                let m = o.meta.expect("wikisource meta");
                if p.ns == "104" {
                    let Some(idx) = page_index(&p.title) else {
                        run.count("page_title_without_scan_index");
                        return Ok(true);
                    };
                    let f = format!("wikisource:index:{}", key(idx));
                    let g = m.indexes.get(idx).and_then(|x| x.author.clone()).map(|a| format!("author:{a}")).unwrap_or_else(|| f.clone());
                    (f, g, WIKISOURCE, "historical_transcription")
                } else {
                    let root = root_title(&p.title);
                    let f = format!("wikisource:work:{}", key(root));
                    let a = m.works.get(&p.title).and_then(|w| w.author.clone()).or_else(|| m.works.get(root).and_then(|w| w.author.clone()));
                    let g = a.map(|a| format!("author:{a}")).unwrap_or_else(|| f.clone());
                    (f, g, WIKISOURCE, "historical_transcription")
                }
            }
        };
        let unit = format!("{}:page:{}", o.project.adapter(), p.id);
        if !run.sampled_meta_grouped(&group, &family, || json!({"page_id": p.id, "ns": p.ns, "revision_id": p.revision_id, "timestamp": p.timestamp,
                "revision_text_sha256": text::sha_hex(p.text.as_bytes()), "title_sha256": text::sha_hex(p.title.as_bytes()), "unit_id": unit}))? {
            return Ok(true);
        }
        if let Some(r) = general_screen(&p) {
            if r == "redirect" {
                run.count(r);
            } else {
                run.reject(&unit, r)?;
            }
            return Ok(true);
        }
        let lt = p.title.to_lowercase();
        if o.project == Project::Wikibooks && PRINT_VERSION_SUFFIXES.iter().any(|s| lt.ends_with(s)) {
            run.reject(&unit, "print_version_page")?;
            return Ok(true);
        }
        // Rights, date and composition metadata.
        let mut composition_year = None;
        let mut licence_basis = "page_licence_template";
        let licence = match o.project {
            Project::Wikivoyage | Project::Wikibooks => {
                licence_basis = "project_licence_cc_by_sa_3_0_for_pre_2023_revisions";
                Some("CC-BY-SA-3.0".to_owned())
            }
            Project::Wikisource => {
                let m = o.meta.expect("wikisource meta");
                if p.ns == "104" {
                    let idx = page_index(&p.title).unwrap_or_default();
                    let Some(x) = m.indexes.get(idx) else {
                        run.reject(&unit, "scan_index_metadata_missing")?;
                        return Ok(true);
                    };
                    if x.language.as_deref().is_some_and(|l| !l.is_empty() && l != "en") {
                        run.reject(&unit, "scan_index_language_not_en")?;
                        return Ok(true);
                    }
                    if x.translator {
                        run.reject(&unit, "translated_work")?;
                        return Ok(true);
                    }
                    composition_year = x.year;
                    match (&x.licence, x.year) {
                        (Some(l), _) => {
                            licence_basis = "licence_template_of_transcluding_main_namespace_page";
                            Some(l.clone())
                        }
                        (None, Some(y)) if y <= 1926 => {
                            licence_basis = "scan_index_publication_year_before_1927_us_public_domain";
                            Some("public-domain-us".to_owned())
                        }
                        _ => None,
                    }
                } else {
                    let own = m.works.get(&p.title).cloned().unwrap_or_default();
                    let root = m.works.get(root_title(&p.title)).cloned().unwrap_or_default();
                    if own.translator || root.translator {
                        run.reject(&unit, "translated_work")?;
                        return Ok(true);
                    }
                    if own.disambiguation {
                        run.reject(&unit, "disambiguation_or_versions_page")?;
                        return Ok(true);
                    }
                    composition_year = own.year.or(root.year);
                    if own.licence.is_none() && root.licence.is_some() {
                        licence_basis = "licence_template_of_root_work_page";
                    }
                    own.licence.or(root.licence)
                }
            }
        };
        let Some(licence) = licence else {
            run.reject(&unit, "no_admitted_licence_or_public_domain_basis")?;
            return Ok(true);
        };
        // Render.
        let is_page = o.project == Project::Wikisource && p.ns == "104";
        let rendered = std::panic::catch_unwind(|| {
            if is_page {
                let (level, body) = page_body(&p.text).ok_or("page_without_proofread_markup")?;
                if level < 3 {
                    return Err(match level {
                        0 => "page_without_text",
                        1 => "page_not_proofread",
                        _ => "page_problematic",
                    });
                }
                let mut paras = page_paras(body);
                let prose_idx: Vec<usize> = (0..paras.len()).filter(|&i| !paras[i].text.is_empty()).collect();
                if let (Some(&a), Some(&b)) = (prose_idx.first(), prose_idx.last()) {
                    paras[a] = Para::non_prose(String::new(), "page_boundary_paragraph");
                    paras[b] = Para::non_prose(String::new(), "page_boundary_paragraph");
                }
                Ok((level, paras))
            } else {
                Ok((0, page_paras(&p.text)))
            }
        });
        let (level, mut paras) = match rendered {
            Err(_) => {
                run.reject(&unit, "renderer_panic")?;
                return Ok(true);
            }
            Ok(Err(r)) => {
                run.reject(&unit, r)?;
                return Ok(true);
            }
            Ok(Ok(x)) => x,
        };
        if o.project == Project::Wikisource {
            // A paragraph that starts in lowercase continues one split by a
            // page-break template or scan boundary; it is not a whole paragraph.
            for x in paras.iter_mut() {
                if x.prose && x.text.chars().next().is_some_and(char::is_lowercase) {
                    *x = Para::non_prose(std::mem::take(&mut x.text), "starts_lowercase_continuation");
                }
            }
        }
        for x in &paras {
            if let Some(r) = x.reason {
                *breaks.entry(r).or_default() += 1;
            }
        }
        let Some(ex) = text::paragraph_run(&paras) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            return Ok(true);
        };
        if let Some(r) = excerpt_screen(&ex) {
            run.reject(&unit, r)?;
            return Ok(true);
        }
        let flags = text::transcription_artifacts(&ex.text);
        for f in &flags {
            *weak.entry(f).or_default() += 1;
        }
        *by_ns.entry(p.ns.clone()).or_default() += 1;
        let rev_url = format!("https://{host}/w/index.php?oldid={}", p.revision_id);
        let history = format!("https://{host}/w/index.php?curid={}&action=history", p.id);
        let notes = match o.project {
            Project::Wikivoyage => "Collaborative travel guide; individual human identity unavailable. Pre-2023 text is CC BY-SA 3.0; part of it descends from Wikitravel (CC BY-SA 3.0) and keeps that attribution through the page history.",
            Project::Wikibooks => "Collaborative textbook or recipe page; individual human identity unavailable. Pre-2023 text is CC BY-SA 3.0 (and GFDL).",
            Project::Wikisource => "Human-proofread transcription of a published work. The work is public domain or freely licensed per the recorded basis; Wikisource's own additions are CC BY-SA. Original composition year is recorded separately from the transcription revision date.",
        };
        let mut rights = record::rights(
            &licence,
            format!("{} contributors, {}, {rev_url}; history {history}", match o.project { Project::Wikivoyage => "Wikivoyage", Project::Wikibooks => "Wikibooks", Project::Wikisource => "Wikisource" }, p.title),
            o.project.copyright_page(),
            notes,
        );
        rights["licence_basis"] = json!(licence_basis);
        rights["redistribute_text"] = json!(false);
        rights["admission_decision"] = json!("default_2026-10-27: public domain, CC0, CC BY and CC BY-SA; pre-cutoff Wikimedia dump revisions; text kept in ignored storage and not redistributed by this pool");
        let id = format!("pool-{}-{}-r{}", o.project.adapter(), p.id, p.revision_id);
        let rank = text::h64(format!("{CAP_DOMAIN}{unit}").as_bytes());
        let h = Held {
            rank,
            unit: unit.clone(),
            id,
            collection,
            domain,
            source_group: family,
            group: group.clone(),
            excerpt: ex,
            method: if is_page { page_method.clone() } else { method.clone() },
            unit_v: json!({"kind": if is_page { "wikisource_page_transcription" } else { "mediawiki_revision" }, "namespace": p.ns,
                           "revision_sha1_base36": p.sha1, "revision_text_sha256": text::sha_hex(p.text.as_bytes()),
                           "proofread_level": is_page.then_some(level),
                           "transcription_artifacts": flags, "transcription_artifact_method": text::TRANSCRIPTION_ARTIFACT_METHOD}),
            provenance: json!({"source_url": rev_url, "history_url": history, "dump": o.dump.name, "dump_url": o.dump.url, "dump_sha256": o.dump.sha256,
                               "page_id": p.id, "namespace": p.ns, "revision_id": p.revision_id, "parent_revision_id": p.parent_id, "title": p.title,
                               "revision_timestamp": p.timestamp, "composition_year": composition_year, "bucket_key": group,
                               "bucket": crate::adapter::bucket(&group)}),
            date: p.timestamp.clone(),
            date_basis: "latest_revision_timestamp_in_pre_cutoff_dump",
            rights,
            aliases: vec![format!("{}_title:{}", o.project.adapter(), p.title)],
        };
        let v = held.entry(group).or_default();
        v.push(h);
        if v.len() > o.max_per_group {
            v.sort_by_key(|x| x.rank);
            let dropped = v.pop().expect("over cap");
            run.reject(&dropped.unit, "group_cap_not_selected")?;
        }
        Ok(true)
    })?;
    let groups = held.len();
    let mut per_collection: BTreeMap<&str, u64> = BTreeMap::new();
    for (_, mut v) in held {
        v.sort_by_key(|x| x.rank);
        for h in v {
            let doc = Doc {
                id: h.id,
                collection: h.collection,
                domain: h.domain,
                source_group: h.source_group,
                writer_key: Some(h.group),
                evidence: "historical_proxy",
                excerpt: &h.excerpt,
                extraction_method: &h.method,
                unit: h.unit_v,
                provenance: h.provenance,
                date: Some(h.date),
                date_basis: h.date_basis,
                cutoff_policy: "dated_before_cutoff",
                rights: h.rights,
                aliases: h.aliases,
            };
            if run.accept(&h.unit, &doc)? {
                *per_collection.entry(h.collection).or_default() += 1;
            }
        }
    }
    Ok(json!({"project": o.project.adapter(), "groups_with_candidates": groups, "max_per_group": o.max_per_group,
              "cap_rule": format!("lowest h64(\"{}\\0\" + unit id) within each split group", CAP_DOMAIN.trim_end_matches('\0')),
              "candidates_by_collection": per_collection, "candidates_by_namespace": by_ns, "paragraph_breaks": breaks,
              "recorded_transcription_artifacts_in_candidates": weak,
              "transcription_artifact_policy": "strong markers (long s, ligatures, soft hyphen, not-sign, split line-end hyphenation, bracketed editorial notes, OCR confusion tokens) reject the candidate; digit_inside_word is recorded only",
              "grouping": match o.project {
                  Project::Wikivoyage => "family and split group: page",
                  Project::Wikibooks => "family: page; split group: book (title before the first '/'); Cookbook recipes: their own page group",
                  Project::Wikisource => "family: work (root title) or scan index; split group: author from work metadata, else the family",
              },
              "limits": ["Collaborative pages: the last editor is not the author and individual identity is unavailable.",
                         "Only the dump's latest pre-cutoff revision is read.",
                         "The conservative renderer rejects any paragraph with inline templates, so pages heavy in templates are under-represented."]}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml(pages: &str) -> String {
        format!("<mediawiki xmlns=\"http://www.mediawiki.org/xml/export-0.10/\"><siteinfo><sitename>Synthetic</sitename></siteinfo>{pages}</mediawiki>")
    }
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }
    fn page(id: u32, ns: u32, title: &str, ts: &str, body: &str) -> String {
        format!("<page><title>{}</title><ns>{ns}</ns><id>{id}</id><revision><id>{}</id><parentid>1</parentid><timestamp>{ts}</timestamp><contributor><username>x</username><id>9</id></contributor><model>wikitext</model><format>text/x-wiki</format><text bytes=\"1\" xml:space=\"preserve\">{}</text><sha1>abc</sha1></revision></page>", esc(title), id * 10, esc(body))
    }
    /// Synthetic English prose with enough function words to pass the screen.
    fn prose(n: usize) -> String {
        let base = ["the", "road", "to", "the", "old", "harbour", "is", "quiet", "and", "it", "was", "built", "in", "a", "dry", "year"];
        let w: Vec<&str> = (0..n - 1).map(|i| base[i % base.len()]).collect();
        format!("{} end.", w.join(" "))
    }
    fn run_on(project: Project, x: &str, meta: Option<&Meta>, cap: usize) -> (Value, Vec<Value>) {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("o");
        let mut run = Run::start(&out, project.adapter(), project.collection(), json!({}), None).unwrap();
        let info = DumpInfo { name: "synthetic".into(), sha256: None, url: None };
        let extra = adapt(x.as_bytes(), &Options { project, dump: &info, max_per_group: cap, meta }, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        let rows = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<Vec<_>>>().unwrap();
        (rep, rows)
    }

    #[test]
    fn template_helpers() {
        let t = "{{header\n | title = A Tale\n | author = [[Author:Jane Roe|Jane Roe]]\n | year = c. 1871\n | translator = \n}}\n{{PD-old}}";
        let ts = templates(t);
        assert_eq!(ts.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["header", "pd-old"]);
        assert_eq!(param(ts[0].1, "year").as_deref(), Some("c. 1871"));
        assert_eq!(param(ts[0].1, "author").as_deref().and_then(author_name).as_deref(), Some("jane_roe"));
        assert_eq!(year_of("c. 1871"), Some(1871));
        assert_eq!(year_of("12345"), None);
        assert_eq!(licence_from_templates(["pd-old"].into_iter()).as_deref(), Some("public-domain-us"));
        assert_eq!(licence_from_templates(["cc-by-sa-3.0"].into_iter()).as_deref(), Some("CC-BY-SA-3.0"));
        assert_eq!(licence_from_templates(["cc-by-nc-4.0"].into_iter()), None);
        assert_eq!(licence_from_templates(["pd-usgov"].into_iter()).as_deref(), Some("public-domain-us-federal-government"));
        assert_eq!(page_index("Page:Some Book (1890).djvu/45"), Some("Some Book (1890).djvu"));
        assert_eq!(page_index("Page:Plate.jpg"), None);
        assert_eq!(transcluded_indexes("<pages index=\"A B.djvu\" from=3 to=9 />"), vec!["A B.djvu"]);
        let (lv, body) = page_body("<noinclude><pagequality level=\"4\" user=\"x\" />HEAD</noinclude>BODY TEXT<noinclude>FOOT</noinclude>").unwrap();
        assert_eq!((lv, body), (4, "BODY TEXT"));
    }

    #[test]
    fn wikivoyage_takes_a_section_run_and_screens() {
        let body = format!("{{{{pagebanner}}}}\n'''Synthtown''' {}\n\n== Understand ==\n{}\n\n{}\n\n== See ==\n* {{{{see|name=Museum}}}}\n", prose(30), prose(120), prose(110));
        let x = xml(&[
            page(1, 0, "Synthtown", "2021-05-01T00:00:00Z", &body),
            page(2, 0, "Latertown", "2023-01-01T00:00:00Z", &body),
            page(3, 0, "Copytown", "2021-05-01T00:00:00Z", &format!("{body}{{{{copyvio}}}}")),
            page(4, 2, "User:Somebody", "2021-05-01T00:00:00Z", &body),
            page(5, 0, "Hyphentown", "2021-05-01T00:00:00Z", &format!("== Understand ==\n{} exam- ple.\n\n{}", prose(120), prose(110))),
        ].concat());
        let (rep, rows) = run_on(Project::Wikivoyage, &x, None, 1);
        assert_eq!(rep["candidates"], 1, "{rep}");
        assert_eq!(rep["rejection_reasons"]["revision_not_before_2022_11_30"], 1);
        assert_eq!(rep["rejection_reasons"]["source_notice_requires_separate_review"], 1);
        assert_eq!(rep["rejection_reasons"]["split_line_end_hyphenation"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["namespace_not_in_scope"], 1);
        assert_eq!(rows[0]["collection"], WIKIVOYAGE);
        assert_eq!(rows[0]["writer_key"], "wikivoyage:page:1");
        assert_eq!(rows[0]["word_count"], 230);
        assert_eq!(rows[0]["rights"]["license"], "CC-BY-SA-3.0");
        assert_eq!(rows[0]["rights"]["redistribute_text"], false);
        assert_eq!(rows[0]["rights"]["licence_basis"], "project_licence_cc_by_sa_3_0_for_pre_2023_revisions");
    }

    #[test]
    fn wikibooks_groups_by_book_caps_and_splits_cookbook() {
        let body = format!("{}\n\n{}", prose(120), prose(110));
        let x = xml(&[
            page(1, 0, "Synthetic Cooking/Chapter 1", "2020-01-01T00:00:00Z", &body),
            page(2, 0, "Synthetic Cooking/Chapter 2", "2020-01-01T00:00:00Z", &body.replace("road", "lane")),
            page(3, 0, "Synthetic Cooking/Chapter 3", "2020-01-01T00:00:00Z", &body.replace("road", "path")),
            page(4, 0, "Synthetic Cooking/Print version", "2020-01-01T00:00:00Z", &body),
            page(5, 102, "Cookbook:Bean Soup", "2020-01-01T00:00:00Z", &body.replace("road", "pot")),
        ].concat());
        let (rep, rows) = run_on(Project::Wikibooks, &x, None, 2);
        assert_eq!(rep["candidates"], 3, "{rep}");
        assert_eq!(rep["rejection_reasons"]["group_cap_not_selected"], 1);
        assert_eq!(rep["rejection_reasons"]["print_version_page"], 1);
        let book: Vec<_> = rows.iter().filter(|r| r["collection"] == WIKIBOOKS).collect();
        assert_eq!(book.len(), 2);
        assert!(book.iter().all(|r| r["writer_key"] == "wikibooks:book:synthetic_cooking"));
        let cook: Vec<_> = rows.iter().filter(|r| r["collection"] == COOKBOOK).collect();
        assert_eq!(cook[0]["domain"], "recipe");
        assert_eq!(cook[0]["writer_key"], "wikibooks:cookbook_page:5");
    }

    #[test]
    fn wikisource_two_passes_page_and_main_namespace() {
        let lower = prose;
        let prose = |n: usize| format!("T{}", &lower(n)[1..]);
        let three = format!("{}\n\n{}\n\n{}\n\n{}", prose(40), prose(120), prose(110), prose(40));
        let page_text = |lv: u8, t: &str| format!("<noinclude><pagequality level=\"{lv}\" user=\"x\" />{{{{rh|1|HEAD|}}}}</noinclude>{t}<noinclude>{{{{smallrefs}}}}</noinclude>");
        let x = xml(&[
            page(1, 106, "Index:Old Book.djvu", "2015-01-01T00:00:00Z", "{{:MediaWiki:Proofreadpage_index_template\n|Type=book\n|Title=[[Old Book]]\n|Author=[[Author:Jane Roe|Jane Roe]]\n|Translator=\n|Year=1880\n|Language=en\n}}"),
            page(2, 106, "Index:New Book.djvu", "2015-01-01T00:00:00Z", "{{:MediaWiki:Proofreadpage_index_template\n|Author=Sam Poe\n|Year=1960\n}}"),
            page(3, 104, "Page:Old Book.djvu/5", "2016-01-01T00:00:00Z", &page_text(4, &three)),
            page(4, 104, "Page:Old Book.djvu/6", "2016-01-01T00:00:00Z", &page_text(1, &three.replace("road", "lane"))),
            page(5, 104, "Page:New Book.djvu/7", "2016-01-01T00:00:00Z", &page_text(4, &three.replace("road", "path"))),
            page(6, 0, "A Story/Chapter 1", "2016-01-01T00:00:00Z", &format!("{{{{header|title=A Story|author=Ann Doe|year=1850}}}}\n{}\n\n{}", prose(120), prose(110).replace("road", "hill"))),
            page(7, 0, "A Story", "2016-01-01T00:00:00Z", "{{header|title=A Story|author=Ann Doe|year=1850}}\nContents only.\n{{PD-old}}"),
            page(8, 0, "Unlicensed", "2016-01-01T00:00:00Z", &format!("{}\n\n{}", prose(120), prose(110).replace("road", "moor"))),
            page(9, 0, "A Story/Chapter 2", "2016-01-01T00:00:00Z", &format!("{{{{page break|7}}}}\n{}\n\n{}", lower(120).replace("road", "fen"), prose(110).replace("road", "bog"))),
        ].concat());
        let d = tempfile::tempdir().unwrap();
        let m = wikisource_meta(x.as_bytes(), &d.path().join("m")).unwrap();
        assert_eq!((m["indexes"].as_u64(), m["work_pages"].as_u64()), (Some(2), Some(4)));
        let meta_text = std::fs::read_to_string(d.path().join("m/meta.jsonl")).unwrap();
        assert!(!meta_text.contains("harbour"), "metadata rows carry no prose");
        let meta = Meta::load(&d.path().join("m/meta.jsonl")).unwrap();
        let (rep, rows) = run_on(Project::Wikisource, &x, Some(&meta), 5);
        assert_eq!(rep["candidates"], 2, "{rep}");
        assert_eq!(rep["rejection_reasons"]["no_paragraph_run_within_bounds"], 2, "root contents page and lowercase continuation");
        assert_eq!(rep["extra"]["paragraph_breaks"]["starts_lowercase_continuation"], 1);
        assert_eq!(rep["rejection_reasons"]["page_not_proofread"], 1);
        assert_eq!(rep["rejection_reasons"]["no_admitted_licence_or_public_domain_basis"], 2);
        let pg = rows.iter().find(|r| r["provenance"]["namespace"] == "104").unwrap();
        assert_eq!(pg["writer_key"], "author:jane_roe");
        assert_eq!(pg["source_group"], "wikisource:index:old_book_djvu");
        assert_eq!(pg["rights"]["license"], "public-domain-us");
        assert_eq!(pg["provenance"]["composition_year"], 1880);
        assert_eq!(pg["word_count"], 230, "first and last page paragraphs are dropped");
        let main = rows.iter().find(|r| r["provenance"]["namespace"] == "0").unwrap();
        assert_eq!(main["rights"]["licence_basis"], "licence_template_of_root_work_page");
        assert_eq!(main["writer_key"], "author:ann_doe");
    }
}

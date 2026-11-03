//! Background sections of US pre-grant patent application publications,
//! read from the USPTO bulk product "Patent Application Full-Text Data (No
//! Images)" (APPXML weekly zips, Open Data Portal).
//!
//! * **Formats.** The weekly files concatenate one XML document per
//!   publication. Publications of 2001-2004 use the `pap-v15`/`pap-v16`
//!   DTDs (`patent-application-publication`); later ones use the ST.36-based
//!   `us-patent-application` DTDs (v4.0 onward). Sequence-listing documents
//!   in the same files are skipped. Documents are parsed without their DTD:
//!   named entities are decoded with a fixed table (XML, HTML/ISO 8879 text
//!   characters and ISO Greek `ISOgrk1`), and any other named entity leaves
//!   a gap, so its paragraph is never admitted.
//! * **Date rule.** The application's own filing date and the publication
//!   date must both precede [`LLM_ERA_BOUND`] (2019-01-01, before GPT-2).
//!   Continuations, divisionals and CIPs are judged by their own filing
//!   date, never the priority date. A document with a missing or
//!   unparseable filing or publication date is rejected. Filing,
//!   publication and earliest priority dates are recorded per row.
//! * **Section.** The first heading naming a background section
//!   (`BACKGROUND`, `BACKGROUND OF THE INVENTION`, `BACKGROUND ART`, ...) up
//!   to the next heading that is not a background subheading (field, related
//!   or prior art). A heading combining background and summary rejects the
//!   unit. Subheadings, claims, tables, formulas, chemistry, images, lists,
//!   superscripts, subscripts and tagged citations never enter the text: a
//!   paragraph holding any of them breaks the run.
//! * **Excerpt.** One excerpt per application: the shared whole-paragraph
//!   run (150..=450 words, ending at the first paragraph boundary at or
//!   after 220 words); otherwise the earliest start whose whole paragraphs
//!   stay under 450 words, completed with the leading whole sentences of the
//!   next paragraph until 220 words are reached (never more than 450).
//! * **Screens.** Applications whose text mentions copyright or a mask work
//!   (37 CFR 1.71(d)-(e) notices) are rejected whole. Paragraph screens: the
//!   shared prose screen, the lost-character `?` screen, symbol residue,
//!   unbalanced brackets and OCR artefacts (broken line-end hyphenation,
//!   digit-for-letter confusions inside words, spaces before commas). The
//!   excerpt passes the shared English screen.
//! * **Groups.** Unit and adapter family: the application number. Bucket
//!   and `writer_key`: an opaque key of the practitioner or correspondent
//!   named in the bibliographic data (publications through early 2011
//!   carry a correspondence address), else the first
//!   inventor, else the first applicant, else the assignee. Family links
//!   (parent, child, provisional, PCT and foreign priority numbers, earlier
//!   publications) are recorded so `uspto-select` can join application
//!   families across the whole pool. An application's own key
//!   (`uspto_app:N`) and the `us_app:N` key printed by related applications
//!   are joined as one key.
//! * **Translations.** A joined family that names a foreign-priority or PCT
//!   receiving office outside [`ENGLISH_FILING_OFFICES`] is dropped whole by
//!   `uspto-select`: its US text is probably a translation. EP and IB count
//!   as non-English because their filing language is not recorded. Foreign
//!   applications that print no priority or PCT number are not detected.
use crate::{
    adapter::Run,
    io::{self, HashingReader},
    pile,
    record::{self, Doc},
    text::{self, Excerpt, GAP, Para, Screen},
};
use anyhow::{Context, Result};
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

pub const COLLECTION: &str = "uspto_application_background_historical";
pub const DOMAIN: &str = "patent_background";
pub const COHORT: &str = "uspto-appxml-backgrounds-v1";
/// Published patent applications are public records (35 U.S.C. 122(b));
/// the USPTO states patent text is typically not subject to copyright
/// restrictions apart from 37 CFR 1.71(d)-(e) notices, which are excluded
/// here; the Open Data Portal listing carries the Public Domain Mark.
pub const LICENSE: &str = "uspto-published-application-public-record;odp-public-domain-mark;no-1.71-notice";
pub const EVIDENCE_URL: &str = "https://www.uspto.gov/terms-use-uspto-websites";
/// No filing or publication on or after this date (GPT-2 was released in
/// February 2019; earlier automated drafting tools were not LLM-based).
pub const LLM_ERA_BOUND: &str = "2019-01-01";
pub const CONTRIBUTOR_DOMAIN: &str = "slop_ninja_a_uspto_contributor_v1";
pub const EXTRACTION: &str = "uspto_appxml_background_section_v1";
pub const EXCERPT_RULE: &str = "whole_paragraph_run_else_whole_paragraphs_plus_leading_whole_sentences_to_220_words_max450_v1";
pub const OCR_SCREEN: &str = "uspto_ocr_artefact_paragraph_screen_v1";
pub const COPYRIGHT_SCREEN: &str = "uspto_copyright_or_mask_work_mention_rejects_application_v1";
/// Filing offices whose filings are taken to be written in English. Any
/// other foreign-priority or PCT receiving office (including EP and IB,
/// whose filing language is not recorded) marks the joined family as a
/// probable translation, and `uspto-select` drops it whole.
pub const ENGLISH_FILING_OFFICES: &[&str] = &["US", "GB", "IE", "AU", "NZ", "CA", "IN", "SG", "ZA"];
pub const TRANSLATION_SCREEN: &str = "uspto_family_non_english_office_priority_or_pct_excluded_v1";
pub const FAMILY_JOIN: &str = "uspto_family_union_us_app_normalised_v2";
pub const AUTHORSHIP_BASIS: &str = "patent specification prepared for the applicant, usually by a registered patent attorney or agent; the writer key groups by the named practitioner or correspondent where the bibliographic data names one, else the first inventor, applicant or assignee; a grouping key, not an authorship claim";

/// Opaque contributor key: never the raw name.
pub fn contributor_key(prefix: &str, raw: &str) -> String {
    let h = text::sha_hex(format!("{CONTRIBUTOR_DOMAIN}\0{prefix}\0{raw}").as_bytes());
    format!("{prefix}:{}", &h[..20])
}

fn bump(m: &mut BTreeMap<String, u64>, k: &str) {
    *m.entry(k.to_owned()).or_default() += 1;
}

// ---------------------------------------------------------------- entities

/// Named entities decoded as text characters. XML's five, the HTML/ISO 8879
/// names that denote ordinary text characters, and ISO Greek (ISOgrk1).
/// Anything else (formula operators, box drawing, invisible operators)
/// leaves a gap.
pub fn entity_char(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "lsqb" => '[',
        "rsqb" => ']',
        "lcub" => '{',
        "rcub" => '}',
        "lpar" => '(',
        "rpar" => ')',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "hyphen" => '\u{2010}',
        "deg" => '\u{B0}',
        "equals" => '=',
        "plus" => '+',
        "minus" => '\u{2212}',
        "times" => '\u{D7}',
        "divide" => '\u{F7}',
        "plusmn" => '\u{B1}',
        "prime" => '\u{2032}',
        "Prime" => '\u{2033}',
        "emsp" => '\u{2003}',
        "ensp" => '\u{2002}',
        "thinsp" => '\u{2009}',
        "nbsp" => '\u{A0}',
        "num" => '#',
        "percnt" => '%',
        "dollar" => '$',
        "ast" => '*',
        "sol" => '/',
        "bsol" => '\\',
        "verbar" => '|',
        "commat" => '@',
        "excl" => '!',
        "quest" => '?',
        "colon" => ':',
        "semi" => ';',
        "comma" => ',',
        "period" => '.',
        "lowbar" => '_',
        "reg" => '\u{AE}',
        "trade" => '\u{2122}',
        "copy" => '\u{A9}',
        "middot" => '\u{B7}',
        "bull" => '\u{2022}',
        "hellip" => '\u{2026}',
        "sect" => '\u{A7}',
        "para" => '\u{B6}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "tilde" => '\u{2DC}',
        "angst" => '\u{C5}',
        "micro" => '\u{B5}',
        "cent" => '\u{A2}',
        "pound" => '\u{A3}',
        "yen" => '\u{A5}',
        "permil" => '\u{2030}',
        "frac12" => '\u{BD}',
        "frac14" => '\u{BC}',
        "frac34" => '\u{BE}',
        "frac13" => '\u{2153}',
        "frac23" => '\u{2154}',
        "frac15" => '\u{2155}',
        "frac25" => '\u{2156}',
        "frac35" => '\u{2157}',
        "frac45" => '\u{2158}',
        "frac16" => '\u{2159}',
        "frac56" => '\u{215A}',
        "frac18" => '\u{215B}',
        "frac38" => '\u{215C}',
        "frac58" => '\u{215D}',
        "frac78" => '\u{215E}',
        "rarr" => '\u{2192}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "darr" => '\u{2193}',
        "infin" => '\u{221E}',
        "le" | "leq" => '\u{2264}',
        "ge" | "geq" => '\u{2265}',
        "ne" => '\u{2260}',
        "ap" => '\u{2248}',
        "equiv" => '\u{2261}',
        // Latin letters with diacritics.
        "aacute" => 'á', "agrave" => 'à', "acirc" => 'â', "auml" => 'ä', "atilde" => 'ã', "aring" => 'å', "aelig" => 'æ',
        "eacute" => 'é', "egrave" => 'è', "ecirc" => 'ê', "euml" => 'ë',
        "iacute" => 'í', "igrave" => 'ì', "icirc" => 'î', "iuml" => 'ï',
        "oacute" => 'ó', "ograve" => 'ò', "ocirc" => 'ô', "ouml" => 'ö', "otilde" => 'õ', "oslash" => 'ø',
        "uacute" => 'ú', "ugrave" => 'ù', "ucirc" => 'û', "uuml" => 'ü',
        "yacute" => 'ý', "yuml" => 'ÿ', "ntilde" => 'ñ', "ccedil" => 'ç', "szlig" => 'ß',
        "Aacute" => 'Á', "Agrave" => 'À', "Acirc" => 'Â', "Auml" => 'Ä', "Atilde" => 'Ã', "Aring" => 'Å', "AElig" => 'Æ',
        "Eacute" => 'É', "Egrave" => 'È', "Ecirc" => 'Ê', "Euml" => 'Ë',
        "Iacute" => 'Í', "Igrave" => 'Ì', "Icirc" => 'Î', "Iuml" => 'Ï',
        "Oacute" => 'Ó', "Ograve" => 'Ò', "Ocirc" => 'Ô', "Ouml" => 'Ö', "Otilde" => 'Õ', "Oslash" => 'Ø',
        "Uacute" => 'Ú', "Ugrave" => 'Ù', "Ucirc" => 'Û', "Uuml" => 'Ü', "Ntilde" => 'Ñ', "Ccedil" => 'Ç',
        // Greek, HTML names.
        "alpha" => 'α', "beta" => 'β', "gamma" => 'γ', "delta" => 'δ', "epsi" | "epsilon" => 'ε', "zeta" => 'ζ', "eta" => 'η',
        "theta" => 'θ', "iota" => 'ι', "kappa" => 'κ', "lambda" => 'λ', "mu" => 'μ', "nu" => 'ν', "xi" => 'ξ', "omicron" => 'ο',
        "pi" => 'π', "rho" => 'ρ', "sigma" => 'σ', "tau" => 'τ', "upsilon" => 'υ', "phi" => 'φ', "chi" => 'χ', "psi" => 'ψ', "omega" => 'ω',
        "Gamma" => 'Γ', "Delta" => 'Δ', "Theta" => 'Θ', "Lambda" => 'Λ', "Xi" => 'Ξ', "Pi" => 'Π', "Sigma" => 'Σ', "Upsilon" => 'Υ',
        "Phi" => 'Φ', "Psi" => 'Ψ', "Omega" => 'Ω',
        // ISO 8879 Greek (ISOgrk1).
        "agr" => 'α', "bgr" => 'β', "ggr" => 'γ', "dgr" => 'δ', "egr" => 'ε', "zgr" => 'ζ', "eegr" => 'η', "thgr" => 'θ', "igr" => 'ι',
        "kgr" => 'κ', "lgr" => 'λ', "mgr" => 'μ', "ngr" => 'ν', "xgr" => 'ξ', "ogr" => 'ο', "pgr" => 'π', "rgr" => 'ρ', "sgr" => 'σ',
        "sfgr" => 'ς', "tgr" => 'τ', "ugr" => 'υ', "phgr" => 'φ', "khgr" => 'χ', "psgr" => 'ψ', "ohgr" => 'ω',
        "Agr" => 'Α', "Bgr" => 'Β', "Ggr" => 'Γ', "Dgr" => 'Δ', "Egr" => 'Ε', "Zgr" => 'Ζ', "EEgr" => 'Η', "THgr" => 'Θ', "Igr" => 'Ι',
        "Kgr" => 'Κ', "Lgr" => 'Λ', "Mgr" => 'Μ', "Ngr" => 'Ν', "Xgr" => 'Ξ', "Ogr" => 'Ο', "Pgr" => 'Π', "Rgr" => 'Ρ', "Sgr" => 'Σ',
        "Tgr" => 'Τ', "Ugr" => 'Υ', "PHgr" => 'Φ', "KHgr" => 'Χ', "PSgr" => 'Ψ', "OHgr" => 'Ω',
        _ => return None,
    })
}

/// Rewrite named entities as numeric references (the XML five stay); an
/// unknown name becomes a gap reference and is counted by name.
pub fn decode_named_entities(s: &str, unknown: &mut BTreeMap<String, u64>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail.char_indices().take(40).find(|&(_, c)| c == ';').map(|(k, _)| k);
        match end {
            Some(k) if k > 0 && !tail.starts_with('#') && tail[..k].chars().all(|c| c.is_ascii_alphanumeric()) => {
                let name = &tail[..k];
                if ["amp", "lt", "gt", "quot", "apos"].contains(&name) {
                    out.push('&');
                    out.push_str(name);
                    out.push(';');
                } else {
                    match entity_char(name) {
                        Some(c) => out.push_str(&format!("&#{};", c as u32)),
                        None => {
                            bump(unknown, name);
                            out.push_str(&format!("&#{};", GAP as u32));
                        }
                    }
                }
                rest = &tail[k + 1..];
            }
            _ => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- documents

/// Concatenated XML documents of one weekly file, split at each line that
/// starts an XML declaration.
pub struct DocSplitter<R> {
    r: R,
    pending: Option<String>,
    done: bool,
}

impl<R: BufRead> DocSplitter<R> {
    pub fn new(r: R) -> Self {
        DocSplitter { r, pending: None, done: false }
    }
}

impl<R: BufRead> Iterator for DocSplitter<R> {
    type Item = Result<String>;
    fn next(&mut self) -> Option<Result<String>> {
        if self.done {
            return None;
        }
        let mut doc = self.pending.take().unwrap_or_default();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match self.r.read_until(b'\n', &mut buf) {
                Ok(0) => {
                    self.done = true;
                    return (!doc.trim().is_empty()).then_some(Ok(doc));
                }
                Ok(_) => {
                    let line = match String::from_utf8(std::mem::take(&mut buf)) {
                        Ok(l) => l,
                        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
                    };
                    // A declaration line directly after another (a doubled
                    // declaration) stays with the same document.
                    if line.starts_with("<?xml") && doc.lines().any(|l| !l.trim().is_empty() && !l.starts_with("<?xml")) {
                        self.pending = Some(line);
                        return Some(Ok(doc));
                    }
                    doc.push_str(&line);
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e.into()));
                }
            }
        }
    }
}

/// YYYYMMDD -> ISO date, validated.
pub fn iso8(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (y, m, d): (u32, u32, u32) = (s[..4].parse().ok()?, s[4..6].parse().ok()?, s[6..].parse().ok()?);
    if !(1790..=2100).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(m - 1) as usize];
    (1..=dim).contains(&d).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn path<'a, 'i>(n: Node<'a, 'i>, names: &[&str]) -> Option<Node<'a, 'i>> {
    names.iter().try_fold(n, |n, k| child(n, k))
}

fn text_at(n: Node, names: &[&str]) -> Option<String> {
    path(n, names).map(all_text).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

fn all_text(n: Node) -> String {
    n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect()
}

/// Folded name for keys: lowercase alphanumeric tokens.
pub fn fold_name(s: &str) -> String {
    text::folded(s)
}

/// Normalised family-link key of one referenced document, or `None` when
/// the number cannot be linked reliably.
pub fn link_key(kind: &str, country: &str, number: &str) -> Option<String> {
    let number = number.split('(').next().unwrap_or("").trim();
    let up = number.to_ascii_uppercase();
    let alnum: String = up.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if up.starts_with("PCT") || kind == "pct" {
        // PCT/US97/05183, PCT/US1997/005183 -> pct:US97:5183
        let rest = up.trim_start_matches("PCT").trim_start_matches(['/', ' ', ':']);
        let mut parts = rest.split(['/', ' ']).filter(|p| !p.is_empty());
        let cy = parts.next()?;
        let num = parts.next()?;
        if cy.len() < 4 || !cy[..2].chars().all(|c| c.is_ascii_alphabetic()) {
            return None;
        }
        let year: String = cy[2..].chars().filter(|c| c.is_ascii_digit()).collect();
        let year = if year.len() == 4 { year[2..].to_owned() } else { year };
        let n = num.trim_start_matches('0');
        return (year.len() == 2 && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| format!("pct:{}{}:{}", &cy[..2], year, n));
    }
    let cc = country.trim().to_ascii_uppercase();
    let foreign = !cc.is_empty() && cc != "US";
    if foreign {
        let n = alnum.trim_start_matches(cc.as_str());
        let digits = n.chars().filter(|c| c.is_ascii_digit()).count();
        return (digits >= 5).then(|| format!("prio:{cc}:{n}"));
    }
    match kind {
        "patent" => {
            let n = alnum.trim_start_matches("US").trim_start_matches('0').to_owned();
            (n.len() >= 5).then(|| format!("us_pat:{n}"))
        }
        "publication" => {
            let d: String = alnum.chars().filter(|c| c.is_ascii_digit()).collect();
            (d.len() == 11).then(|| format!("us_pub:{d}"))
        }
        _ => {
            let d: String = alnum.trim_start_matches("US").chars().filter(|c| c.is_ascii_digit()).collect();
            (d.len() == 8 && d.len() == alnum.trim_start_matches("US").len()).then(|| format!("us_app:{d}"))
        }
    }
}

/// Bibliographic facts of one publication. Names are only ever hashed.
#[derive(Debug, Default, Clone)]
pub struct Bib {
    pub format: &'static str,
    pub dtd: String,
    pub lang: String,
    pub pub_number: String,
    pub kind: String,
    pub pub_date_raw: String,
    pub app_number: String,
    pub filing_date_raw: String,
    pub appl_type: String,
    pub links: BTreeSet<String>,
    pub link_dates: Vec<String>,
    pub unlinkable_refs: u64,
    pub practitioner: Option<String>,
    pub first_inventor: Option<String>,
    pub first_applicant: Option<String>,
    pub assignee: Option<String>,
    pub title_sha: String,
}

fn person(ab: Node) -> Option<String> {
    let org = text_at(ab, &["orgname"]).or_else(|| text_at(ab, &["name"]));
    let last = text_at(ab, &["last-name"]);
    let first = text_at(ab, &["first-name"]);
    match (org, last) {
        (Some(o), _) => Some(o),
        (None, Some(l)) => Some(format!("{l}\0{}", first.unwrap_or_default())),
        _ => None,
    }
}

fn person_place(ab: Node) -> String {
    let city = text_at(ab, &["city"]).or_else(|| text_at(ab, &["address", "city"])).unwrap_or_default();
    let cc = text_at(ab, &["country"]).or_else(|| text_at(ab, &["address", "country"])).unwrap_or_default();
    format!("{}\0{}", fold_name(&city), fold_name(&cc))
}

fn collect_links(n: Node, bib: &mut Bib) {
    for d in n.descendants().filter(|d| d.is_element() && d.tag_name().name() == "document-id") {
        let anc: Vec<&str> = d.ancestors().filter(|a| a.is_element()).map(|a| a.tag_name().name()).collect();
        let kind = if anc.iter().any(|a| matches!(*a, "parent-grant-document" | "parent-patent" | "reissue" | "reexamination")) {
            "patent"
        } else if anc.iter().any(|a| matches!(*a, "parent-pct-document" | "pct-application" | "a-371-of-international")) {
            "pct"
        } else if anc.contains(&"related-publication") {
            "publication"
        } else {
            "application"
        };
        let num = text_at(d, &["doc-number"]).unwrap_or_default();
        let cc = text_at(d, &["country"]).or_else(|| text_at(d, &["country-code"])).unwrap_or_default();
        let kind = if kind == "application" && num.to_ascii_uppercase().starts_with("PCT") { "pct" } else { kind };
        match link_key(kind, &cc, &num) {
            Some(k) => {
                bib.links.insert(k);
            }
            None => bib.unlinkable_refs += 1,
        }
        if let Some(dt) = text_at(d, &["date"]).or_else(|| text_at(d, &["document-date"])).and_then(|s| iso8(&s)) {
            bib.link_dates.push(dt);
        }
    }
}

/// Bibliographic data of a `us-patent-application` (v4.x) document.
pub fn bib_v4(root: Node) -> Bib {
    let mut b = Bib { format: "us-patent-application-v4", lang: root.attribute("lang").unwrap_or("").to_owned(), ..Default::default() };
    let Some(bd) = child(root, "us-bibliographic-data-application") else { return b };
    if let Some(d) = path(bd, &["publication-reference", "document-id"]) {
        b.pub_number = text_at(d, &["doc-number"]).unwrap_or_default();
        b.kind = text_at(d, &["kind"]).unwrap_or_default();
        b.pub_date_raw = text_at(d, &["date"]).unwrap_or_default();
    }
    if let Some(a) = child(bd, "application-reference") {
        b.appl_type = a.attribute("appl-type").unwrap_or("").to_owned();
        if let Some(d) = child(a, "document-id") {
            b.app_number = text_at(d, &["doc-number"]).unwrap_or_default();
            b.filing_date_raw = text_at(d, &["date"]).unwrap_or_default();
        }
    }
    b.title_sha = text::sha_hex(text_at(bd, &["invention-title"]).unwrap_or_default().as_bytes());
    if let Some(r) = child(bd, "us-related-documents") {
        collect_links(r, &mut b);
    }
    if let Some(pc) = child(bd, "priority-claims") {
        for c in pc.children().filter(|c| c.is_element() && c.tag_name().name() == "priority-claim") {
            let cc = text_at(c, &["country"]).unwrap_or_default();
            let num = text_at(c, &["doc-number"]).unwrap_or_default();
            match link_key(if c.attribute("kind") == Some("international") { "pct" } else { "priority" }, &cc, &num) {
                Some(k) => {
                    b.links.insert(k);
                }
                None => b.unlinkable_refs += 1,
            }
            if let Some(dt) = text_at(c, &["date"]).and_then(|s| iso8(&s)) {
                b.link_dates.push(dt);
            }
        }
    }
    if let Some(pct) = child(bd, "pct-or-regional-filing-data") {
        collect_links(pct, &mut b);
    }
    let parties = child(bd, "parties").or_else(|| child(bd, "us-parties"));
    if let Some(p) = parties {
        let agent = path(p, &["agents", "agent", "addressbook"]).and_then(person);
        let corr = path(p, &["correspondence-address", "addressbook"]).and_then(person);
        b.practitioner = agent.or(corr);
        let applicants: Vec<Node> = p
            .descendants()
            .filter(|n| n.is_element() && matches!(n.tag_name().name(), "applicant" | "us-applicant"))
            .collect();
        let inventors: Vec<Node> = p.descendants().filter(|n| n.is_element() && n.tag_name().name() == "inventor").collect();
        let inv = inventors.first().copied().or_else(|| applicants.iter().copied().find(|a| a.attribute("app-type").is_some_and(|t| t.contains("inventor"))));
        b.first_inventor = inv.and_then(|n| child(n, "addressbook")).and_then(|ab| person(ab).map(|nm| format!("{nm}\0{}", person_place(ab))));
        b.first_applicant = applicants.first().and_then(|n| child(*n, "addressbook")).and_then(person);
    }
    // Post-2012 filings may name the owning organisation as applicant
    // instead of (or as well as) an assignee.
    b.assignee = path(bd, &["assignees", "assignee", "addressbook"]).and_then(person).or_else(|| {
        parties?.descendants().filter(|n| n.is_element() && matches!(n.tag_name().name(), "applicant" | "us-applicant")).find_map(|n| child(n, "addressbook").and_then(|ab| text_at(ab, &["orgname"])))
    });
    b
}

/// Bibliographic data of a `patent-application-publication` (pap v1.5/1.6).
pub fn bib_pap(root: Node) -> Bib {
    let mut b = Bib { format: "patent-application-publication-pap", lang: "EN".into(), ..Default::default() };
    let Some(bd) = child(root, "subdoc-bibliographic-information") else { return b };
    if let Some(d) = child(bd, "document-id") {
        b.pub_number = text_at(d, &["doc-number"]).unwrap_or_default();
        b.kind = text_at(d, &["kind-code"]).unwrap_or_default();
        b.pub_date_raw = text_at(d, &["document-date"]).unwrap_or_default();
    }
    b.appl_type = text_at(bd, &["publication-filing-type"]).unwrap_or_default();
    if let Some(f) = child(bd, "domestic-filing-data") {
        b.app_number = text_at(f, &["application-number", "doc-number"]).unwrap_or_default();
        b.filing_date_raw = text_at(f, &["filing-date"]).unwrap_or_default();
    }
    b.title_sha = text::sha_hex(text_at(bd, &["technical-information", "title-of-invention"]).unwrap_or_default().as_bytes());
    if let Some(c) = child(bd, "continuity-data") {
        collect_links(c, &mut b);
    }
    if let Some(ic) = child(bd, "international-conventions") {
        collect_links(ic, &mut b);
    }
    for fp in bd.children().filter(|c| c.is_element() && c.tag_name().name() == "foreign-priority-data") {
        let cc = text_at(fp, &["country-code"]).unwrap_or_default();
        let num = text_at(fp, &["priority-application-number", "doc-number"]).unwrap_or_default();
        match link_key("priority", &cc, &num) {
            Some(k) => {
                b.links.insert(k);
            }
            None => b.unlinkable_refs += 1,
        }
        if let Some(dt) = text_at(fp, &["filing-date"]).and_then(|s| iso8(&s)) {
            b.link_dates.push(dt);
        }
    }
    if let Some(ca) = child(bd, "correspondence-address") {
        let n1 = text_at(ca, &["name-1"]).unwrap_or_default();
        let n2 = text_at(ca, &["name-2"]).unwrap_or_default();
        if !(n1.is_empty() && n2.is_empty()) {
            b.practitioner = Some(format!("{n1}\0{n2}"));
        }
    }
    if let Some(inv) = path(bd, &["inventors", "first-named-inventor"]) {
        let nm = child(inv, "name");
        let fam = nm.and_then(|n| text_at(n, &["family-name"])).unwrap_or_default();
        let giv = nm.and_then(|n| text_at(n, &["given-name"])).unwrap_or_default();
        let res = child(inv, "residence").and_then(|r| r.children().find(|c| c.is_element()));
        let city = res.and_then(|r| text_at(r, &["city"])).unwrap_or_default();
        let cc = res.and_then(|r| text_at(r, &["country-code"])).unwrap_or_default();
        if !fam.is_empty() || !giv.is_empty() {
            b.first_inventor = Some(format!("{fam}\0{giv}\0{}\0{}", fold_name(&city), fold_name(&cc)));
        }
    }
    if let Some(a) = child(bd, "assignee") {
        b.assignee = text_at(a, &["organization-name"]).or_else(|| {
            child(a, "name").map(|n| format!("{}\0{}", text_at(n, &["family-name"]).unwrap_or_default(), text_at(n, &["given-name"]).unwrap_or_default()))
        });
    }
    b
}

/// Writer key and its basis.
pub fn writer_key(b: &Bib) -> Option<(String, &'static str)> {
    let fold = |s: &str| s.split('\0').map(fold_name).collect::<Vec<_>>().join("\0");
    let nonempty = |s: &String| !fold(s).replace('\0', "").is_empty();
    if let Some(p) = b.practitioner.as_ref().filter(|s| nonempty(s)) {
        return Some((contributor_key("uspto_practitioner", &fold(p)), "practitioner_or_correspondent"));
    }
    if let Some(p) = b.first_inventor.as_ref().filter(|s| nonempty(s)) {
        return Some((contributor_key("uspto_first_inventor", &fold(p)), "first_inventor"));
    }
    if let Some(p) = b.first_applicant.as_ref().filter(|s| nonempty(s)) {
        return Some((contributor_key("uspto_first_applicant", &fold(p)), "first_applicant"));
    }
    b.assignee.as_ref().filter(|s| nonempty(s)).map(|p| (contributor_key("uspto_assignee", &fold(p)), "assignee"))
}

pub fn assignee_key(b: &Bib) -> Option<String> {
    let f: String = b.assignee.as_deref()?.split('\0').map(fold_name).collect::<Vec<_>>().join("\0");
    (!f.replace('\0', "").is_empty()).then(|| contributor_key("uspto_assignee", &f))
}

// ---------------------------------------------------------------- headings

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Head {
    Background,
    BackgroundWithSummary,
    Sub,
    Other,
}

/// Lowercase words of a heading, leading enumeration (`1.`, `II.`, `A)`) removed.
pub fn heading_words(h: &str) -> Vec<String> {
    let mut w: Vec<String> = text::folded_tokens(h);
    while let Some(f) = w.first() {
        let roman = f.chars().all(|c| matches!(c, 'i' | 'v' | 'x')) && f.len() <= 4;
        if f.chars().all(|c| c.is_ascii_digit()) || (f.len() == 1 && f.chars().all(|c| c.is_ascii_alphabetic()) && w.len() > 1) || (roman && w.len() > 1) {
            w.remove(0);
        } else {
            break;
        }
    }
    w
}

pub fn classify_heading(h: &str) -> Head {
    let w = heading_words(h);
    let s = w.join(" ");
    let has = |x: &str| w.iter().any(|t| t == x);
    let ends_section = has("summary") || s.contains("detailed description") || has("drawings") || has("drawing") || has("claims") || has("objects") || has("object") || s.contains("brief description of the invention");
    if has("background") {
        if has("summary") || s.contains("detailed description") || has("drawings") || has("objects") {
            return Head::BackgroundWithSummary;
        }
        return if w.len() <= 12 { Head::Background } else { Head::Other };
    }
    let sub = has("field") || s.contains("related art") || s.contains("prior art") || s.contains("state of the art") || s.contains("relevant art") || s.contains("conventional art")
        || s.contains("related technology") || s.contains("description of the art") || s.contains("conventional technology") || s.contains("related arts");
    if sub && !ends_section && w.len() <= 10 { Head::Sub } else { Head::Other }
}

// ---------------------------------------------------------------- paragraphs

/// Inline elements whose text is kept; everything else not listed in
/// `DROP` leaves a gap.
const KEEP: &[&str] = &["b", "i", "u", "smallcaps", "figref", "crossref", "bold", "italic", "underline", "highlight", "cross-reference", "emphasis"];
const DROP: &[&str] = &["number"];

/// Render a `p` (v4) or `paragraph` (pap) element. Unknown or excluded
/// elements and processing instructions (in-line formulae) leave a gap.
pub fn render_para(n: Node, gaps: &mut BTreeMap<String, u64>) -> String {
    fn walk(n: Node, out: &mut String, gaps: &mut BTreeMap<String, u64>) {
        for c in n.children() {
            if c.is_text() {
                out.push_str(c.text().unwrap_or(""));
            } else if c.is_pi() {
                let t = c.pi().map(|p| p.target).unwrap_or("");
                bump(gaps, &format!("pi:{t}"));
                out.push(GAP);
            } else if c.is_element() {
                let name = c.tag_name().name();
                if DROP.contains(&name) {
                    continue;
                }
                if KEEP.contains(&name) {
                    walk(c, out, gaps);
                } else {
                    bump(gaps, name);
                    out.push(GAP);
                }
            }
        }
    }
    let mut s = String::new();
    walk(n, &mut s, gaps);
    s
}

/// A rendered description item in document order.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Heading(String),
    Para(String),
}

pub fn description_items(desc: Node, gaps: &mut BTreeMap<String, u64>) -> Vec<Item> {
    fn walk(n: Node, out: &mut Vec<Item>, gaps: &mut BTreeMap<String, u64>) {
        for c in n.children().filter(|c| c.is_element()) {
            match c.tag_name().name() {
                "heading" => out.push(Item::Heading(text::squash(&all_text(c)))),
                "p" | "paragraph" => out.push(Item::Para(render_para(c, gaps))),
                _ => walk(c, out, gaps),
            }
        }
    }
    let mut out = Vec::new();
    walk(desc, &mut out, gaps);
    out
}

/// The background section: paragraphs after the first background heading
/// up to the next heading that is not a background subheading.
/// Subheadings become non-prose paragraphs.
pub fn background_section(items: &[Item]) -> Result<(String, Vec<String>, Vec<usize>), &'static str> {
    let mut start = None;
    for (i, it) in items.iter().enumerate() {
        if let Item::Heading(h) = it {
            match classify_heading(h) {
                Head::Background => {
                    start = Some((i, h.clone()));
                    break;
                }
                Head::BackgroundWithSummary => return Err("background_heading_combined_with_summary"),
                _ => {}
            }
        }
    }
    let Some((s, head)) = start else { return Err("no_background_heading") };
    let mut paras = Vec::new();
    let mut subs = Vec::new();
    for it in &items[s + 1..] {
        match it {
            Item::Heading(h) => match classify_heading(h) {
                Head::Sub | Head::Background => {
                    subs.push(paras.len());
                    paras.push(h.clone());
                }
                _ => break,
            },
            Item::Para(p) => paras.push(p.clone()),
        }
    }
    if paras.iter().enumerate().all(|(i, _)| subs.contains(&i)) {
        return Err("empty_background_section");
    }
    Ok((head, paras, subs))
}

/// OCR artefact in a paragraph: a line-end hyphenation left inside the
/// text (`inven- tion`), a digit standing for a letter inside a word
/// (`c0mponent`, `a1so`) or two or more spaces before a comma or semicolon.
pub fn ocr_artefact(t: &str) -> Option<&'static str> {
    const JOINERS: &[&str] = &["and", "or", "to", "nor", "through", "versus", "vs", "as", "and/or", "but", "in", "of"];
    let toks: Vec<&str> = t.split_whitespace().collect();
    for w in toks.windows(2) {
        let a = w[0];
        let b = w[1].trim_end_matches(|c: char| c.is_ascii_punctuation());
        if a.len() >= 3 && a.ends_with('-') && !a.ends_with("--") && a[..a.len() - 1].chars().last().is_some_and(|c| c.is_ascii_lowercase())
            && b.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && !JOINERS.contains(&b)
        {
            return Some("ocr_broken_hyphenation");
        }
    }
    for tok in &toks {
        let w = tok.trim_matches(|c: char| !c.is_alphanumeric());
        let b = w.as_bytes();
        for i in 2..b.len().saturating_sub(2) {
            if matches!(b[i], b'0' | b'1' | b'5') && b[i - 2..i].iter().all(u8::is_ascii_lowercase) && b[i + 1..i + 3].iter().all(u8::is_ascii_lowercase) {
                return Some("ocr_digit_inside_word");
            }
        }
    }
    if t.matches(" ,").count() + t.matches(" ;").count() >= 2 {
        return Some("ocr_space_before_comma");
    }
    None
}

pub fn classify_para(raw: &str) -> Para {
    let t = text::squash(raw);
    if t.contains(GAP) {
        return Para::non_prose(t, "removed_inline_content");
    }
    if pile::lost_character_mark(&t, true) {
        return Para::non_prose(t, "lost_character_question_mark");
    }
    if t.contains('_') || pile::math_notation(&t) {
        return Para::non_prose(t, "symbol_or_conversion_residue");
    }
    // `1)` enumerations add closing parentheses; an unclosed opening one or
    // unbalanced square brackets is conversion loss.
    if t.matches('(').count() > t.matches(')').count() || t.matches('[').count() != t.matches(']').count() {
        return Para::non_prose(t, "unbalanced_brackets");
    }
    if let Some(r) = ocr_artefact(&t) {
        return Para::non_prose(t, r);
    }
    if t.contains('|') {
        return Para::non_prose(t, "pipe_residue");
    }
    Para::classify(t, Screen::XML)
}

const ABBREVIATIONS: &[&str] = &[
    "fig", "figs", "no", "nos", "pat", "pats", "inc", "corp", "co", "ltd", "mr", "mrs", "ms", "dr", "prof", "vs", "ser", "appl", "pub", "publ", "vol", "ref", "refs",
    "approx", "ca", "cf", "jan", "feb", "mar", "apr", "jun", "jul", "aug", "sep", "sept", "oct", "nov", "dec", "st", "eq", "eqs", "sec", "ch", "pp", "ed", "eds", "al",
    "esp", "resp", "ex", "art", "para", "paras", "nr", "num", "u.s", "e.g", "i.e", "viz", "etc.,", "jr", "sr", "dept", "univ", "assoc", "intl", "int", "proc", "j",
];

/// Byte offsets just after each sentence end: `.`, `!` or `?` (plus closing
/// quotes or brackets) followed by whitespace and an uppercase letter or an
/// opening quote or bracket, where a `.` does not close an abbreviation, an
/// initial or a dotted token (`U.S.`).
pub fn sentence_ends(t: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let ch: Vec<(usize, char)> = t.char_indices().collect();
    for k in 0..ch.len() {
        let (i, c) = ch[k];
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let mut j = k + 1;
        while j < ch.len() && matches!(ch[j].1, '"' | '\'' | '\u{201D}' | '\u{2019}' | ')' | ']') {
            j += 1;
        }
        let end = if j < ch.len() { ch[j].0 } else { t.len() };
        if j >= ch.len() || !ch[j].1.is_whitespace() {
            continue;
        }
        let mut m = j;
        while m < ch.len() && ch[m].1.is_whitespace() {
            m += 1;
        }
        if m >= ch.len() {
            continue;
        }
        let nx = ch[m].1;
        if !(nx.is_uppercase() || matches!(nx, '"' | '\u{201C}' | '(' | '\u{2018}')) {
            continue;
        }
        if c == '.' {
            let tok_start = t[..i].rfind(char::is_whitespace).map(|p| p + 1).unwrap_or(0);
            let tok = t[tok_start..i].trim_start_matches(['(', '"', '\u{201C}', '[']).to_lowercase();
            let single = tok.chars().count() == 1 && tok.chars().all(char::is_alphabetic);
            if tok.is_empty() || single || tok.contains('.') || ABBREVIATIONS.contains(&tok.as_str()) {
                continue;
            }
        }
        out.push(end);
    }
    out
}

/// One excerpt from the section (see the module rules) and whether its
/// last paragraph was cut at a sentence end.
pub fn background_excerpt(paras: &[Para]) -> Option<(Excerpt, bool)> {
    if let Some(e) = text::paragraph_run(paras) {
        return Some((e, false));
    }
    for i in 0..paras.len() {
        if !paras[i].prose {
            continue;
        }
        let mut parts: Vec<&str> = Vec::new();
        let mut n = 0;
        let mut j = i;
        while j < paras.len() && paras[j].prose {
            let w = text::words(&paras[j].text);
            if n < text::TARGET_WORDS && n + w <= text::MAX_WORDS {
                parts.push(&paras[j].text);
                n += w;
                j += 1;
            } else {
                break;
            }
        }
        if n >= text::TARGET_WORDS || j >= paras.len() || !paras[j].prose {
            continue;
        }
        let p = &paras[j].text;
        for e in sentence_ends(p) {
            let pre = p[..e].trim_end();
            let m = n + text::words(pre);
            if m >= text::TARGET_WORDS {
                if m <= text::MAX_WORDS {
                    let mut all = parts.clone();
                    all.push(pre);
                    let t = all.join("\n\n");
                    return Some((Excerpt { words: text::words(&t), text: t, start: i, end: j + 1, complete_unit: false }, true));
                }
                break;
            }
        }
    }
    None
}

/// Copyright or mask-work notice or mention anywhere in the document text.
pub fn copyright_mention(lower_text: &str) -> bool {
    lower_text.contains("copyright") || lower_text.contains("mask work") || lower_text.contains("mask-work") || lower_text.contains('\u{A9}')
}

/// Earliest priority date: the earliest date of any related document or
/// priority claim, else the filing date.
pub fn priority_date(b: &Bib, filing: &str) -> (String, &'static str) {
    match b.link_dates.iter().filter(|d| d.as_str() <= filing).min() {
        Some(d) if d.as_str() < filing => (d.clone(), "earliest_related_or_priority_document_date"),
        _ => (filing.to_owned(), "own_filing_date_no_earlier_priority"),
    }
}

fn week_date(file: &str) -> Option<String> {
    let stem = file.trim_end_matches(".zip");
    let d = stem.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    (d.len() == 6).then(|| iso8(&format!("20{d}"))).flatten()
}

fn rights(pub_no: &str, kind: &str, app: &str) -> Value {
    let mut r = record::rights(
        LICENSE,
        format!("US patent application publication {pub_no} {kind} (application {app}), background section; source: United States Patent and Trademark Office bulk data (APPXML)"),
        EVIDENCE_URL,
        "Published US patent applications are public records; USPTO terms of use: the text of a patent is typically not subject to copyright restrictions, subject to 37 CFR 1.71(d)-(e) and 1.84(s) notices; applications whose text mentions copyright or a mask work are excluded. The ODP data.gov listing carries the Public Domain Mark 1.0. No product-level research-only term applies to APPXML. Source acknowledgement requested by USPTO is recorded here.",
    );
    r["admission_decision"] = json!("user_2026-10-28: USPTO application backgrounds read directly from USPTO bulk data (not HUPD); filing and publication before 2019-01-01; Train only");
    r["admission_basis"] = json!(pile::ADMISSION_BASIS);
    r["cohort_id"] = json!(COHORT);
    r["removable_cohort"] = json!(true);
    r["redistribute_text"] = json!(false);
    r["evaluation_use"] = json!("train_only");
    r["allowed_splits"] = json!(["train", "train_mining"]);
    r["source_acknowledgement"] = json!("Source: United States Patent and Trademark Office, www.uspto.gov");
    r
}

#[derive(Default)]
struct Stats {
    by_year: BTreeMap<String, u64>,
    writer_basis: BTreeMap<String, u64>,
    gaps: BTreeMap<String, u64>,
    unknown_entities: BTreeMap<String, u64>,
    parse_errors: BTreeMap<String, u64>,
    breaks: BTreeMap<String, u64>,
    breaks_by_year: BTreeMap<String, BTreeMap<String, u64>>,
    headings: BTreeMap<String, u64>,
    formats: BTreeMap<String, u64>,
    cut_at_sentence: u64,
    unlinkable_refs: u64,
    writers: HashSet<String>,
}

/// Adapt weekly APPXML zips. `weeks` are zip paths; a sibling
/// `<zip>.receipt.json` supplies the download URL when present.
pub fn appxml(weeks: &[PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("{EXTRACTION};{COPYRIGHT_SCREEN};{OCR_SCREEN};{};{EXCERPT_RULE}", pile::LOST_CHARACTER_SCREEN);
    let mut st = Stats::default();
    let mut files = Vec::new();
    for w in weeks {
        let fname = w.file_name().and_then(|n| n.to_str()).context("week file name")?.to_owned();
        let wdate = week_date(&fname);
        let receipt: Value = std::fs::read(format!("{}.receipt.json", w.display())).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!(null));
        let url = receipt["url"].as_str().map(str::to_owned);
        let (zsha, zbytes) = io::file_sha(w)?;
        if let Some(rs) = receipt["sha256"].as_str() {
            anyhow::ensure!(rs == zsha, "{fname}: sha256 differs from its receipt");
        }
        let mut z = zip::ZipArchive::new(std::fs::File::open(w).with_context(|| format!("open {}", w.display()))?)?;
        let mut members: Vec<(String, usize)> = (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|e| (e.name().to_owned(), i))).filter(|(n, _)| n.to_ascii_lowercase().ends_with(".xml")).collect();
        members.sort();
        let mut docs_in_file = 0u64;
        let mut cand_in_file = 0u64;
        let mut member_info = Vec::new();
        for (mname, idx) in members {
            let e = z.by_index(idx)?;
            let mut hr = HashingReader::new(e);
            {
                let br = BufReader::with_capacity(1 << 20, &mut hr);
                for (k, doc) in DocSplitter::new(br).enumerate() {
                    if run.full() {
                        break;
                    }
                    let doc = doc?;
                    docs_in_file += 1;
                    if one_doc(&doc, &fname, &mname, k, wdate.as_deref(), url.as_deref(), &zsha, &method, run, &mut st)? {
                        cand_in_file += 1;
                    }
                }
            }
            let (msha, mbytes) = hr.finish();
            member_info.push(json!({"member": mname, "sha256": msha, "bytes": mbytes}));
        }
        files.push(json!({"file": fname, "publication_week": wdate, "url": url, "sha256": zsha, "bytes": zbytes, "members": member_info,
                          "documents": docs_in_file, "candidates": cand_in_file}));
    }
    let mut heads: Vec<(String, u64)> = st.headings.into_iter().collect();
    heads.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    heads.truncate(60);
    Ok(json!({"files": files, "candidates_by_publication_year": st.by_year, "writer_key_basis": st.writer_basis,
              "writer_keys_with_candidates": st.writers.len(), "paragraph_breaks": st.breaks, "paragraph_breaks_by_year": st.breaks_by_year,
              "gap_elements": st.gaps, "unknown_entities": st.unknown_entities, "xml_parse_errors": st.parse_errors, "document_formats": st.formats,
              "background_headings_top": heads, "excerpts_cut_at_sentence_end": st.cut_at_sentence, "unlinkable_family_references": st.unlinkable_refs,
              "date_rule": {"bound": LLM_ERA_BOUND, "rule": "own filing date and publication date both before the bound; priority date recorded only; missing or unparseable dates rejected"},
              "grouping": "unit and adapter family: application number (joined into application families by uspto-select); bucket, split group and writer_key: practitioner or correspondent where named, else first inventor, else first applicant, else assignee (opaque)",
              "authorship_basis": AUTHORSHIP_BASIS,
              "limits": ["publications after early 2011 name no practitioner or correspondent, so their writer key is the first inventor; the key changes kind by year (see writer_key_basis per year in the admission check)",
                         "names are not disambiguated; spelling variants split a writer and namesakes merge",
                         "family links use the numbers printed in each publication; unlinked continuations are caught only by text deduplication",
                         "the background heading names the section; text under other headings is never taken"]}))
}

#[allow(clippy::too_many_arguments)]
fn one_doc(doc: &str, fname: &str, member: &str, k: usize, wdate: Option<&str>, url: Option<&str>, zsha: &str, method: &str, run: &mut Run, st: &mut Stats) -> Result<bool> {
    run.units_seen += 1;
    let body: String = doc.lines().filter(|l| !l.starts_with("<?xml")).collect::<Vec<_>>().join("\n");
    let prepared = decode_named_entities(&crate::xmldoc::without_doctype(&body), &mut st.unknown_entities);
    let parsed = Document::parse_with_options(&prepared, ParsingOptions { allow_dtd: false, ..Default::default() });
    let unit_fallback = format!("uspto_week:{fname}:{k}");
    let xml = match parsed {
        Ok(x) => x,
        Err(e) => {
            let kind = e.to_string();
            let kind = kind.split(" at ").next().unwrap_or("").to_owned();
            bump(&mut st.parse_errors, &kind);
            run.reject(&unit_fallback, "xml_parse_error")?;
            return Ok(false);
        }
    };
    let root = xml.root_element();
    let rname = root.tag_name().name();
    let (bib, desc) = match rname {
        "us-patent-application" => (bib_v4(root), child(root, "description")),
        "patent-application-publication" => (bib_pap(root), child(root, "subdoc-description")),
        _ => {
            run.count(&format!("skipped_root_{rname}"));
            return Ok(false);
        }
    };
    let dtd = doc.find("<!DOCTYPE").and_then(|i| doc[i..].split('"').nth(1)).unwrap_or("").to_owned();
    bump(&mut st.formats, &dtd);
    let unit = if bib.pub_number.is_empty() { unit_fallback } else { format!("uspto_publication:{}{}", bib.pub_number, bib.kind) };
    let app = bib.app_number.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
    if app.is_empty() {
        run.reject(&unit, "no_application_number")?;
        return Ok(false);
    }
    let family = format!("uspto_app:{app}");
    let Some((wk, wbasis)) = writer_key(&bib) else {
        run.reject(&unit, "no_writer_key")?;
        return Ok(false);
    };
    let ak = assignee_key(&bib);
    let mut links = bib.links.clone();
    links.insert(family.clone());
    if let Some(k) = link_key("publication", "US", &bib.pub_number) {
        links.insert(k);
    }
    let filing = iso8(&bib.filing_date_raw);
    let publication = iso8(&bib.pub_date_raw);
    let prio = filing.as_deref().map(|f| priority_date(&bib, f));
    let meta = || {
        json!({"publication_number": bib.pub_number, "kind": bib.kind, "application_number": app, "filing_date": filing, "publication_date": publication,
               "priority_date": prio.as_ref().map(|p| &p.0), "family_links": links, "writer_key": wk, "writer_key_basis": wbasis, "assignee_key": ak,
               "week_file": fname, "document_index": k, "format": bib.format})
    };
    if !run.sampled_meta_grouped(&wk, &family, meta)? {
        return Ok(false);
    }
    let (Some(filing), Some(publication)) = (filing.clone(), publication.clone()) else {
        run.reject(&unit, "filing_or_publication_date_missing_or_unparseable")?;
        return Ok(false);
    };
    if filing.as_str() >= LLM_ERA_BOUND || publication.as_str() >= LLM_ERA_BOUND {
        run.reject(&unit, "filing_or_publication_on_or_after_2019_01_01")?;
        return Ok(false);
    }
    if filing > publication {
        run.reject(&unit, "filing_date_after_publication_date")?;
        return Ok(false);
    }
    if !bib.lang.is_empty() && !bib.lang.eq_ignore_ascii_case("en") {
        run.reject(&unit, "document_language_not_en")?;
        return Ok(false);
    }
    let lower = all_text(root).to_lowercase();
    if copyright_mention(&lower) {
        run.reject(&unit, "copyright_or_mask_work_mention")?;
        return Ok(false);
    }
    let Some(desc) = desc else {
        run.reject(&unit, "no_description")?;
        return Ok(false);
    };
    let items = description_items(desc, &mut st.gaps);
    let (head, raw, subs) = match background_section(&items) {
        Ok(x) => x,
        Err(r) => {
            run.reject(&unit, r)?;
            return Ok(false);
        }
    };
    bump(&mut st.headings, &heading_words(&head).join(" "));
    let year = publication[..4].to_owned();
    let paras: Vec<Para> = raw.iter().enumerate().map(|(i, p)| if subs.contains(&i) { Para::non_prose(text::squash(p), "subheading") } else { classify_para(p) }).collect();
    for p in &paras {
        if let Some(r) = p.reason {
            bump(&mut st.breaks, r);
            bump(st.breaks_by_year.entry(year.clone()).or_default(), r);
        }
    }
    let Some((ex, cut)) = background_excerpt(&paras) else {
        run.reject(&unit, "no_excerpt_within_bounds")?;
        return Ok(false);
    };
    if let Some(r) = text::english_screen(&ex.text) {
        run.reject(&unit, r)?;
        return Ok(false);
    }
    let (prio_date, prio_basis) = prio.unwrap_or_else(|| (filing.clone(), "own_filing_date_no_earlier_priority"));
    let section_text: String = raw.join("\n\n");
    let doc_rec = Doc {
        id: format!("pool-uspto-{}{}", bib.pub_number, bib.kind),
        collection: COLLECTION,
        domain: DOMAIN,
        source_group: family.clone(),
        writer_key: Some(wk.clone()),
        evidence: "historical_proxy",
        excerpt: &ex,
        extraction_method: method,
        unit: json!({"kind": "uspto_application_background_section", "publication_number": bib.pub_number, "publication_kind": bib.kind,
                     "application_number": app, "heading": head, "section_paragraphs": raw.len(), "section_sha256": text::sha_hex(section_text.as_bytes()),
                     "document_sha256": text::sha_hex(doc.as_bytes()), "week_file": fname, "zip_member": member, "document_index": k,
                     "cut_inside_paragraph_at_sentence_end": cut, "span_basis": "background_section_paragraph_index_half_open"}),
        provenance: json!({"publication_number": bib.pub_number, "publication_kind": bib.kind, "application_number": app, "application_type": bib.appl_type,
                           "filing_date": filing, "publication_date": publication, "priority_date": prio_date, "priority_date_basis": prio_basis,
                           "date_rule": format!("max(filing_date, publication_date) < {LLM_ERA_BOUND}"),
                           "family_links": links, "writer_key_basis": wbasis, "assignee_key": ak, "authorship_basis": AUTHORSHIP_BASIS,
                           "title_sha256": bib.title_sha, "format": bib.format, "dtd": dtd, "publication_week": wdate,
                           "release": "USPTO Patent Application Full-Text Data (No Images), APPXML weekly file", "download": url, "week_file_sha256": zsha}),
        date: Some(publication.clone()),
        date_basis: "publication_date",
        cutoff_policy: "dated_before_cutoff",
        rights: rights(&bib.pub_number, &bib.kind, &app),
        aliases: vec![],
    };
    if run.accept(&unit, &doc_rec)? {
        bump(&mut st.by_year, &year);
        bump(&mut st.writer_basis, wbasis);
        st.unlinkable_refs += bib.unlinkable_refs;
        if cut {
            st.cut_at_sentence += 1;
        }
        st.writers.insert(wk);
        return Ok(true);
    }
    Ok(false)
}

// ---------------------------------------------------------------- selection

struct Dsu {
    p: Vec<usize>,
}

impl Dsu {
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.p[r] != r {
            r = self.p[r];
        }
        let mut y = x;
        while self.p[y] != r {
            let n = self.p[y];
            self.p[y] = r;
            y = n;
        }
        r
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.p[a.max(b)] = a.min(b);
        }
    }
}

pub struct SelectOptions {
    pub max_per_writer: usize,
    pub max_per_assignee: usize,
    pub max_per_year: usize,
    /// Folded window length of the near-duplicate screen (0 disables it).
    pub near_window: usize,
    pub seed: String,
}

/// Normalised family key: an application's own adapter key
/// (`uspto_app:N`) and a printed reference to it (`us_app:N`) are one key.
pub fn family_key(k: &str) -> String {
    match k.strip_prefix("uspto_app:") {
        Some(n) if n.len() == 8 && n.bytes().all(|b| b.is_ascii_digit()) => format!("us_app:{n}"),
        _ => k.to_owned(),
    }
}

/// Filing office of a foreign-priority (`prio:CC:N`) or PCT (`pct:CCYY:N`)
/// family key.
pub fn filing_office(k: &str) -> Option<&str> {
    if let Some(r) = k.strip_prefix("prio:") {
        return r.split(':').next().filter(|c| !c.is_empty());
    }
    k.strip_prefix("pct:").and_then(|r| r.get(..2))
}

/// True when the key names a filing office outside [`ENGLISH_FILING_OFFICES`].
pub fn non_english_office(k: &str) -> bool {
    filing_office(k).is_some_and(|c| !ENGLISH_FILING_OFFICES.contains(&c))
}

fn links_of(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

/// Join application families across the pool (adapter candidates and the
/// reserved metadata rows, with `uspto_app:N` and `us_app:N` as one key),
/// drop families with any reserved row and families naming a foreign
/// priority or PCT office outside [`ENGLISH_FILING_OFFICES`] (probable
/// translations), then keep
/// candidates in seeded order while their family is unused and their writer,
/// assignee and publication-year counts are below the caps. Writes an
/// adapter-shaped directory whose `source_group` is the family and a
/// rewritten `reserved-metadata.jsonl` keyed by the same families.
pub fn select(train: &Path, scans: &[PathBuf], out: &Path, opt: &SelectOptions) -> Result<Value> {
    let rep: Value = serde_json::from_slice(&std::fs::read(train.join("report.json"))?)?;
    anyhow::ensure!(rep["schema"] == crate::adapter::REPORT_SCHEMA && rep["complete"] == true, "incomplete adapter report");
    let (csha, _) = io::file_sha(&train.join("candidates.jsonl"))?;
    anyhow::ensure!(rep["candidates_sha256"] == csha.as_str(), "candidates changed since adapter report");
    let mut keys: HashMap<String, usize> = HashMap::new();
    let mut key_names: Vec<String> = Vec::new();
    let intern = |k: &str, keys: &mut HashMap<String, usize>, names: &mut Vec<String>| -> usize {
        if let Some(&i) = keys.get(k) {
            return i;
        }
        names.push(k.to_owned());
        keys.insert(k.to_owned(), names.len() - 1);
        names.len() - 1
    };
    struct C {
        id: String,
        own: usize,
        writer: String,
        assignee: Option<String>,
        year: String,
        rank: u64,
        offset: u64,
    }
    let mut cands: Vec<C> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let cpath = train.join("candidates.jsonl");
    let mut reader = BufReader::with_capacity(1 << 20, std::fs::File::open(&cpath)?);
    let mut offset = 0u64;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let at = offset;
        offset += n as u64;
        if line.trim().is_empty() {
            continue;
        }
        let row: Value = serde_json::from_str(&line).with_context(|| format!("{}: offset {at}", cpath.display()))?;
        let id = row["id"].as_str().context("id")?.to_owned();
        let own = intern(&family_key(row["source_group"].as_str().context("source_group")?), &mut keys, &mut key_names);
        for l in links_of(&row["provenance"]["family_links"]) {
            let k = intern(&family_key(&l), &mut keys, &mut key_names);
            edges.push((own, k));
        }
        let pd = row["provenance"]["publication_date"].as_str().context("publication_date")?;
        cands.push(C {
            rank: text::h64(format!("{}\0{id}", opt.seed).as_bytes()),
            id,
            own,
            writer: row["writer_key"].as_str().context("writer_key")?.to_owned(),
            assignee: row["provenance"]["assignee_key"].as_str().map(str::to_owned),
            year: pd[..4].to_owned(),
            offset: at,
        });
    }
    let mut reserved_rows: Vec<(Value, usize)> = Vec::new();
    let mut scan_info = Vec::new();
    for s in scans {
        let p = s.join("reserved-metadata.jsonl");
        let (sha, _) = io::file_sha(&p)?;
        let mut n = 0u64;
        for row in io::jsonl_rows(&p)? {
            let row = row?;
            anyhow::ensure!(row.get("text").is_none(), "reserved metadata carries text");
            let own = intern(&family_key(row["family_key"].as_str().context("family_key")?), &mut keys, &mut key_names);
            for l in links_of(&row["family_links"]) {
                let k = intern(&family_key(&l), &mut keys, &mut key_names);
                edges.push((own, k));
            }
            reserved_rows.push((row, own));
            n += 1;
        }
        scan_info.push(json!({"path": p, "sha256": sha, "rows": n}));
    }
    let mut dsu = Dsu { p: (0..key_names.len()).collect() };
    for (a, b) in edges {
        dsu.union(a, b);
    }
    // Canonical family key: the smallest member key of the component.
    let mut canon: HashMap<usize, String> = HashMap::new();
    let mut comp_size: HashMap<usize, usize> = HashMap::new();
    for i in 0..key_names.len() {
        let r = dsu.find(i);
        let e = canon.entry(r).or_insert_with(|| key_names[i].clone());
        if key_names[i] < *e {
            *e = key_names[i].clone();
        }
    }
    // Filing offices named anywhere in each joined family.
    let mut offices: HashMap<usize, BTreeSet<String>> = HashMap::new();
    for i in 0..key_names.len() {
        if let Some(c) = filing_office(&key_names[i]) {
            let c = c.to_owned();
            offices.entry(dsu.find(i)).or_default().insert(c);
        }
    }
    let translated = |r: usize| offices.get(&r).is_some_and(|s| s.iter().any(|c| !ENGLISH_FILING_OFFICES.contains(&c.as_str())));
    let translated_families = offices.keys().filter(|&&r| translated(r)).count();
    let mut reserved_comps: HashSet<usize> = HashSet::new();
    for (_, own) in &reserved_rows {
        reserved_comps.insert(dsu.find(*own));
    }
    for c in &cands {
        *comp_size.entry(dsu.find(c.own)).or_default() += 1;
    }
    let mut order: Vec<usize> = (0..cands.len()).collect();
    order.sort_by_key(|&i| (cands[i].rank, cands[i].id.clone()));
    let mut used_family: HashSet<usize> = HashSet::new();
    let mut per_writer: HashMap<String, usize> = HashMap::new();
    let mut per_assignee: HashMap<String, usize> = HashMap::new();
    let mut per_year: BTreeMap<String, usize> = BTreeMap::new();
    let mut avail_year: BTreeMap<String, usize> = BTreeMap::new();
    let mut keep: HashMap<String, String> = HashMap::new();
    let mut reason_of: HashMap<String, &'static str> = HashMap::new();
    let mut windows: HashSet<u64> = HashSet::new();
    let mut file = std::fs::File::open(&cpath)?;
    let mut text_at_offset = |off: u64| -> Result<String> {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(off))?;
        let mut l = String::new();
        BufReader::new(&mut file).read_line(&mut l)?;
        let v: Value = serde_json::from_str(&l)?;
        Ok(v["text"].as_str().context("text")?.to_owned())
    };
    for &i in &order {
        let c = &cands[i];
        *avail_year.entry(c.year.clone()).or_default() += 1;
        let r = dsu.find(c.own);
        let reason = if reserved_comps.contains(&r) {
            Some("family_has_reserved_bucket_rows")
        } else if translated(r) {
            Some("family_names_non_english_filing_office")
        } else if used_family.contains(&r) {
            Some("family_already_selected")
        } else if per_writer.get(&c.writer).copied().unwrap_or(0) >= opt.max_per_writer {
            Some("writer_cap")
        } else if c.assignee.as_ref().is_some_and(|a| per_assignee.get(a).copied().unwrap_or(0) >= opt.max_per_assignee) {
            Some("assignee_cap")
        } else if per_year.get(&c.year).copied().unwrap_or(0) >= opt.max_per_year {
            Some("publication_year_quota")
        } else {
            None
        };
        // Boilerplate and near duplicates: a candidate sharing any folded
        // `near_window`-word window with an already kept candidate is dropped.
        let mut shingles = Vec::new();
        let reason = match reason {
            None if opt.near_window > 0 => {
                shingles = text::shingles_folded_n(&text_at_offset(c.offset)?, opt.near_window);
                shingles.iter().any(|h| windows.contains(h)).then_some("shares_folded_window_with_kept_candidate")
            }
            r => r,
        };
        match reason {
            Some(x) => {
                reason_of.insert(c.id.clone(), x);
            }
            None => {
                windows.extend(shingles);
                used_family.insert(r);
                *per_writer.entry(c.writer.clone()).or_default() += 1;
                if let Some(a) = &c.assignee {
                    *per_assignee.entry(a.clone()).or_default() += 1;
                }
                *per_year.entry(c.year.clone()).or_default() += 1;
                keep.insert(c.id.clone(), canon[&r].clone());
            }
        }
    }
    io::fresh_dir(out)?;
    let mut cw = io::JsonlWriter::create(&out.join("candidates.jsonl"))?;
    let mut rw = io::JsonlWriter::create(&out.join("rejections.jsonl"))?;
    let mut removed: BTreeMap<&str, u64> = BTreeMap::new();
    let mut words = Vec::new();
    let mut basis: BTreeMap<String, u64> = BTreeMap::new();
    for row in io::jsonl_rows(&train.join("candidates.jsonl"))? {
        let mut row = row?;
        let id = row["id"].as_str().unwrap_or("").to_owned();
        if let Some(fam) = keep.get(&id) {
            let unit_family = row["source_group"].clone();
            row["source_group"] = json!(format!("uspto_family:{fam}"));
            row["provenance"]["unit_family_key"] = unit_family;
            let own = keys[&family_key(row["provenance"]["unit_family_key"].as_str().unwrap_or(""))];
            let root = dsu.find(own);
            row["provenance"]["family_component_candidates"] = json!(comp_size.get(&root).copied().unwrap_or(1));
            row["provenance"]["family_join"] = json!(FAMILY_JOIN);
            row["provenance"]["family_filing_offices"] = json!(offices.get(&root).cloned().unwrap_or_default());
            row["provenance"]["translation_screen"] = json!(TRANSLATION_SCREEN);
            words.push(row["word_count"].as_u64().unwrap_or(0));
            bump(&mut basis, row["provenance"]["writer_key_basis"].as_str().unwrap_or(""));
            cw.write(&row)?;
        } else {
            let r = reason_of.get(&id).copied().unwrap_or("not_selected");
            *removed.entry(r).or_default() += 1;
            rw.write(&json!({"unit_id": id, "reason": r}))?;
        }
    }
    let (sha, rows) = cw.finish()?;
    let (rsha, rrows) = rw.finish()?;
    let mut mw = io::JsonlWriter::create(&out.join("reserved-metadata.jsonl"))?;
    for (mut row, own) in reserved_rows {
        let fam = canon[&dsu.find(own)].clone();
        row["unit_family_key"] = row["family_key"].clone();
        row["family_key"] = json!(format!("uspto_family:{fam}"));
        mw.write(&row)?;
    }
    let (msha, mrows) = mw.finish()?;
    let mut sizes: Vec<usize> = comp_size.values().copied().collect();
    sizes.sort_unstable();
    let mut size_hist: BTreeMap<String, u64> = BTreeMap::new();
    for s in &sizes {
        bump(&mut size_hist, &(if *s >= 10 { "10+".to_owned() } else { s.to_string() }));
    }
    let mut wcounts: BTreeMap<String, u64> = BTreeMap::new();
    for n in per_writer.values() {
        bump(&mut wcounts, &n.to_string());
    }
    let mut acounts: BTreeMap<String, u64> = BTreeMap::new();
    for n in per_assignee.values() {
        bump(&mut acounts, &n.to_string());
    }
    words.sort_unstable();
    let mut report = rep.clone();
    let mut reasons = rep["rejection_reasons"].clone();
    for (k, v) in &removed {
        reasons[*k] = json!(v);
    }
    report["candidates"] = json!(rows);
    report["candidates_sha256"] = json!(sha);
    report["rejections_sha256_selection_stage"] = json!(rsha);
    report["rejection_reasons"] = reasons;
    report["finished_utc"] = json!(io::now_utc());
    report["candidate_word_counts"] = if words.is_empty() { json!(null) } else { json!({"min": words[0], "median": words[words.len() / 2], "max": words[words.len() - 1]}) };
    let sel = json!({"input_dir": train, "input_candidates_sha256": csha, "scans": scan_info, "seed": opt.seed,
        "max_per_writer_key": opt.max_per_writer, "max_per_assignee": opt.max_per_assignee, "max_per_publication_year": opt.max_per_year, "near_window": opt.near_window,
        "family_join": FAMILY_JOIN, "translation_screen": TRANSLATION_SCREEN, "english_filing_offices": ENGLISH_FILING_OFFICES,
        "families_naming_non_english_filing_office": translated_families,
        "rule": "families: union of application, parent, child, provisional, PCT, foreign-priority and earlier-publication numbers over candidates and reserved rows, with an application's own key uspto_app:N and printed references us_app:N joined as one key; canonical key = smallest member key; candidates visited by SHA-256(seed NUL id); a candidate is kept iff its family has no reserved row, names no foreign-priority or PCT office outside english_filing_offices (probable translation) and has no kept row, its writer key, assignee and publication year are below their caps, and it shares no folded near_window-word window with a kept candidate",
        "removed_by_reason": removed, "removed": rrows, "kept": rows, "kept_by_publication_year": per_year, "available_by_publication_year": avail_year,
        "kept_writer_key_basis": basis, "families_with_candidates": sizes.len(), "family_candidates_histogram": size_hist,
        "largest_family_candidates": sizes.last(), "writer_keys_kept": per_writer.len(), "rows_per_kept_writer_key": wcounts,
        "assignees_kept": per_assignee.len(), "rows_per_kept_assignee": acounts, "reserved_families": reserved_comps.len(),
        "reserved_metadata": {"file": "reserved-metadata.jsonl", "rows": mrows, "sha256": msha, "rule": "family_key rewritten to the joined family; unit_family_key keeps the adapter family"}});
    report["uspto_selection"] = sel.clone();
    io::write_json(&out.join("report.json"), &report)?;
    Ok(sel)
}

// ---------------------------------------------------------------- admission check

/// Admission assertions on a finished pool: every row of the collection,
/// valid filing and publication dates with max(filing, publication) before
/// the bound, one row per family, writer and assignee caps. Families are
/// re-joined independently of `source_group` from every row's family links
/// and the reserved metadata (normalised keys, [`family_key`]): each joined
/// family holds at most one row, no reserved row, and no foreign-priority or
/// PCT office outside [`ENGLISH_FILING_OFFICES`]. Returns the summary; fails
/// on any violation.
pub fn check(documents: &Path, reserved: &Path, max_per_writer: usize, max_per_assignee: usize) -> Result<Value> {
    let mut jk: HashMap<String, usize> = HashMap::new();
    let mut jn: Vec<String> = Vec::new();
    let mut jedges: Vec<(usize, usize)> = Vec::new();
    let mut doc_own: Vec<(String, usize)> = Vec::new();
    let mut res_own: Vec<usize> = Vec::new();
    let mut add = |own: &str, links: Vec<String>, jk: &mut HashMap<String, usize>, jn: &mut Vec<String>| -> usize {
        let mut id = |k: String| -> usize {
            if let Some(&i) = jk.get(&k) {
                return i;
            }
            jn.push(k.clone());
            jk.insert(k, jn.len() - 1);
            jn.len() - 1
        };
        let o = id(family_key(own));
        for l in links {
            let k = id(family_key(&l));
            jedges.push((o, k));
        }
        o
    };
    for row in io::jsonl_rows(documents)? {
        let row = row?;
        let p = &row["provenance"];
        let o = add(p["unit_family_key"].as_str().context("unit_family_key")?, links_of(&p["family_links"]), &mut jk, &mut jn);
        doc_own.push((row["id"].as_str().unwrap_or("").to_owned(), o));
    }
    let mut reserved_rows = 0u64;
    for row in io::jsonl_rows(reserved)? {
        let row = row?;
        anyhow::ensure!(row.get("text").is_none(), "reserved metadata carries text");
        let own = row["unit_family_key"].as_str().or_else(|| row["family_key"].as_str()).context("reserved family key")?.to_owned();
        res_own.push(add(&own, links_of(&row["family_links"]), &mut jk, &mut jn));
        reserved_rows += 1;
    }
    let mut dsu = Dsu { p: (0..jn.len()).collect() };
    for (a, b) in jedges {
        dsu.union(a, b);
    }
    let reserved_roots: HashSet<usize> = res_own.iter().map(|&o| dsu.find(o)).collect();
    let mut bad_office: HashSet<usize> = HashSet::new();
    for i in 0..jn.len() {
        if non_english_office(&jn[i]) {
            bad_office.insert(dsu.find(i));
        }
    }
    let mut seen_roots: HashMap<usize, String> = HashMap::new();
    for (id, o) in &doc_own {
        let r = dsu.find(*o);
        anyhow::ensure!(!reserved_roots.contains(&r), "row {id}: joined family has a reserved-bucket row");
        anyhow::ensure!(!bad_office.contains(&r), "row {id}: joined family names a non-English filing office");
        if let Some(other) = seen_roots.insert(r, id.clone()) {
            anyhow::bail!("rows {other} and {id}: one joined family");
        }
    }
    let mut fams: HashSet<String> = HashSet::new();
    let mut writers: HashMap<String, usize> = HashMap::new();
    let mut assignees: HashMap<String, usize> = HashMap::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut basis: BTreeMap<String, u64> = BTreeMap::new();
    let mut basis_by_year: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut max_date = String::new();
    let mut cut = 0u64;
    let mut n = 0u64;
    let mut words: Vec<u64> = Vec::new();
    for (i, row) in io::jsonl_rows(documents)?.enumerate() {
        let row = row?;
        n += 1;
        let id = row["id"].as_str().unwrap_or("");
        anyhow::ensure!(row["collection"] == COLLECTION, "row {i} ({id}): collection");
        anyhow::ensure!(row["rights"]["license"] == LICENSE && row["rights"]["redistribute_text"] == false, "row {i} ({id}): rights");
        let p = &row["provenance"];
        let f = p["filing_date"].as_str().filter(|d| record::valid_date(d)).with_context(|| format!("row {i} ({id}): filing_date"))?;
        let d = p["publication_date"].as_str().filter(|d| record::valid_date(d)).with_context(|| format!("row {i} ({id}): publication_date"))?;
        anyhow::ensure!(p["priority_date"].as_str().is_some_and(record::valid_date), "row {i} ({id}): priority_date");
        let m = f.max(d);
        anyhow::ensure!(m < LLM_ERA_BOUND, "row {i} ({id}): max(filing_date, publication_date) = {m} is not before {LLM_ERA_BOUND}");
        if m > max_date.as_str() {
            max_date = m.to_owned();
        }
        let fam = row["source_group"].as_str().context("source_group")?.to_owned();
        anyhow::ensure!(fams.insert(fam.clone()), "row {i} ({id}): second row of family {fam}");
        let w = row["writer_key"].as_str().context("writer_key")?.to_owned();
        let e = writers.entry(w).or_default();
        *e += 1;
        anyhow::ensure!(*e <= max_per_writer, "row {i} ({id}): writer cap exceeded");
        if let Some(a) = p["assignee_key"].as_str() {
            let e = assignees.entry(a.to_owned()).or_default();
            *e += 1;
            anyhow::ensure!(*e <= max_per_assignee, "row {i} ({id}): assignee cap exceeded");
        }
        let wc = row["word_count"].as_u64().unwrap_or(0);
        anyhow::ensure!((text::MIN_WORDS as u64..=text::MAX_WORDS as u64).contains(&wc), "row {i} ({id}): word count");
        words.push(wc);
        let y = d[..4].to_owned();
        bump(&mut by_year, &y);
        let b = p["writer_key_basis"].as_str().unwrap_or("").to_owned();
        bump(&mut basis, &b);
        bump(basis_by_year.entry(y).or_default(), &b);
        if row["extraction"]["unit"]["cut_inside_paragraph_at_sentence_end"] == true {
            cut += 1;
        }
    }
    anyhow::ensure!(n > 0, "empty pool");
    let mut wc: BTreeMap<String, u64> = BTreeMap::new();
    for v in writers.values() {
        bump(&mut wc, &v.to_string());
    }
    let mut ac: BTreeMap<String, u64> = BTreeMap::new();
    for v in assignees.values() {
        bump(&mut ac, &v.to_string());
    }
    words.sort_unstable();
    Ok(json!({"schema": "slop_ninja_a_uspto_admission_check_v1", "rows": n, "passed": true,
              "assertions": [format!("every row: valid filing_date, publication_date and priority_date; max(filing_date, publication_date) < {LLM_ERA_BOUND}"),
                             "one row per joined application family (source_group)",
                             "families re-joined from family links and reserved metadata (uspto_app:N = us_app:N): at most one row per family, no reserved-bucket row, no foreign-priority or PCT office outside english_filing_offices",
                             format!("at most {max_per_writer} rows per writer_key and {max_per_assignee} per assignee_key"),
                             "collection, licence id and redistribute_text: false on every row", "150..=450 whitespace words"],
              "rejoin": {"family_join": FAMILY_JOIN, "translation_screen": TRANSLATION_SCREEN, "english_filing_offices": ENGLISH_FILING_OFFICES,
                         "reserved_metadata_rows": reserved_rows, "joined_keys": jn.len()},
              "latest_filing_or_publication_date": max_date, "rows_by_publication_year": by_year, "writer_key_basis": basis,
              "writer_key_basis_by_year": basis_by_year, "writer_keys": writers.len(), "rows_per_writer_key": wc,
              "assignee_keys": assignees.len(), "rows_per_assignee_key": ac, "rows_without_assignee": n - assignees.values().sum::<usize>() as u64,
              "excerpts_cut_at_sentence_end": cut,
              "word_counts": {"min": words[0], "median": words[words.len() / 2], "max": words[words.len() - 1]}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(x: &str) -> (String, BTreeMap<String, u64>) {
        let mut u = BTreeMap::new();
        (decode_named_entities(x, &mut u), u)
    }

    #[test]
    fn entities_decode_exactly_or_leave_a_gap() {
        let (s, u) = parse("<p>&lsqb;0001&rsqb; 10 &mgr;m &amp; &ldquo;x&rdquo; &Sum; &it;</p>");
        let d = Document::parse(&s).unwrap();
        assert_eq!(all_text(d.root_element()), format!("[0001] 10 μm & “x” {GAP} {GAP}"));
        assert_eq!(u.get("Sum"), Some(&1));
        assert_eq!(u.get("it"), Some(&1));
        assert_eq!(parse("a &amp; b &#38; c &#x26;").0, "a &amp; b &#38; c &#x26;");
    }

    #[test]
    fn splitter_splits_concatenated_documents() {
        let x = "<?xml version=\"1.0\"?>\n<a>1</a>\n<?xml version=\"1.0\"?>\n<b>2</b>\n";
        let docs: Vec<String> = DocSplitter::new(x.as_bytes()).map(Result::unwrap).collect();
        assert_eq!(docs, vec!["<?xml version=\"1.0\"?>\n<a>1</a>\n", "<?xml version=\"1.0\"?>\n<b>2</b>\n"]);
        let x = "<?xml version=\"1.0\"?>\n<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<a>1</a>\n";
        assert_eq!(DocSplitter::new(x.as_bytes()).count(), 1);
    }

    #[test]
    fn dates_and_links() {
        assert_eq!(iso8("20180229"), None);
        assert_eq!(iso8("20160229").as_deref(), Some("2016-02-29"));
        assert_eq!(iso8(""), None);
        assert_eq!(link_key("application", "US", "10/221,222").as_deref(), Some("us_app:10221222"));
        assert_eq!(link_key("application", "", "894,495"), None);
        assert_eq!(link_key("application", "US", "PCT/US97/05183").as_deref(), Some("pct:US97:5183"));
        assert_eq!(link_key("pct", "WO", "PCT/US1997/005183").as_deref(), Some("pct:US97:5183"));
        assert_eq!(link_key("priority", "JP", "2003-102671 (PATE").as_deref(), Some("prio:JP:2003102671"));
        assert_eq!(link_key("patent", "US", "6769139").as_deref(), Some("us_pat:6769139"));
        assert_eq!(link_key("publication", "US", "20050000001").as_deref(), Some("us_pub:20050000001"));
    }

    #[test]
    fn headings() {
        for h in ["BACKGROUND OF THE INVENTION", "BACKGROUND", "2. Background of the Invention", "BACKGROUND ART", "FIELD AND BACKGROUND OF THE INVENTION", "III. BACKGROUND", "Background of the Invention 1. Field of the Invention"] {
            assert_eq!(classify_heading(h), Head::Background, "{h}");
        }
        for h in ["BACKGROUND AND SUMMARY OF THE INVENTION", "Background and Summary"] {
            assert_eq!(classify_heading(h), Head::BackgroundWithSummary, "{h}");
        }
        for h in ["1. Field of the Invention", "Description of the Related Art", "2. Description of the Prior Art", "TECHNICAL FIELD"] {
            assert_eq!(classify_heading(h), Head::Sub, "{h}");
        }
        for h in ["SUMMARY OF THE INVENTION", "BRIEF DESCRIPTION OF THE DRAWINGS", "DETAILED DESCRIPTION", "OBJECTS OF THE INVENTION", "CROSS-REFERENCE TO RELATED APPLICATIONS"] {
            assert_eq!(classify_heading(h), Head::Other, "{h}");
        }
    }

    fn sentence(n: usize) -> String {
        "The device described in U.S. Pat. No. 5,123,456 includes a frame and a cover. ".repeat(n)
    }

    #[test]
    fn sentence_ends_skip_abbreviations() {
        let t = "See U.S. Pat. No. 5,123,456. The frame (FIG. 1) is shown. Then e.g. Smith et al. Teach this. Done.";
        let ends: Vec<&str> = sentence_ends(t).into_iter().map(|e| &t[..e]).collect();
        assert_eq!(ends, vec!["See U.S. Pat. No. 5,123,456.", "See U.S. Pat. No. 5,123,456. The frame (FIG. 1) is shown.", "See U.S. Pat. No. 5,123,456. The frame (FIG. 1) is shown. Then e.g. Smith et al. Teach this."]);
    }

    #[test]
    fn excerpt_whole_paragraphs_then_sentence_cut() {
        let short = classify_para(&sentence(8)); // 112 words
        let long = classify_para(&sentence(40)); // 560 words
        assert!(short.prose && long.prose, "{:?} {:?}", short.reason, long.reason);
        let (e, cut) = background_excerpt(&[short.clone(), short.clone()]).unwrap();
        assert!(!cut && e.words == 224);
        let (e, cut) = background_excerpt(&[short.clone(), long.clone()]).unwrap();
        assert!(cut && e.words >= 220 && e.words <= 450 && e.end == 2, "{}", e.words);
        assert!(e.text.ends_with("cover."));
        let (e, cut) = background_excerpt(&[long.clone()]).unwrap();
        assert!(cut && (220..=240).contains(&e.words), "{}", e.words);
        assert!(background_excerpt(&[short]).is_none());
    }

    #[test]
    fn paragraph_screens() {
        assert_eq!(ocr_artefact("the inven- tion relates to a device for holding things."), Some("ocr_broken_hyphenation"));
        assert_eq!(ocr_artefact("pre- and post-processing steps are used in the method."), None);
        assert_eq!(ocr_artefact("a comp0nent of the device is shown."), Some("ocr_digit_inside_word"));
        assert_eq!(ocr_artefact("water (H2O) and MP3 files and Ti6Al4V alloys are known."), None);
        assert_eq!(ocr_artefact("a frame , a cover , and a base are provided."), Some("ocr_space_before_comma"));
        assert_eq!(classify_para(&format!("{}{GAP} and more text here.", sentence(1))).reason, Some("removed_inline_content"));
        assert_eq!(classify_para("Alzheimer?s disease affects many people in the world today and tomorrow.").reason, Some("lost_character_question_mark"));
        assert!(copyright_mention("a portion of the disclosure of this patent document contains material which is subject to copyright protection."));
        assert!(!copyright_mention("the invention relates to digital rights management of media."));
    }

    #[test]
    fn v4_document_end_to_end() {
        let x = r#"<us-patent-application lang="EN"><us-bibliographic-data-application>
<publication-reference><document-id><doc-number>20120144541</doc-number><kind>A1</kind><date>20120614</date></document-id></publication-reference>
<application-reference appl-type="utility"><document-id><doc-number>12968075</doc-number><date>20101214</date></document-id></application-reference>
<invention-title>T</invention-title>
<us-related-documents><continuation><relation><parent-doc><document-id><country>US</country><doc-number>12000001</doc-number><date>20080101</date></document-id></parent-doc>
<child-doc><document-id><country>US</country><doc-number>12968075</doc-number></document-id></child-doc></relation></continuation></us-related-documents>
<parties><applicants><applicant app-type="applicant-inventor"><addressbook><last-name>Doe</last-name><first-name>Jane</first-name><address><city>X</city><country>US</country></address></addressbook></applicant></applicants></parties>
</us-bibliographic-data-application>
<description><heading level="1">TECHNICAL FIELD</heading><p>Field.</p><heading level="1">BACKGROUND OF THE INVENTION</heading>
<heading level="2">1. Field of the Invention</heading><p>A short field paragraph.</p><p>Body <b>one</b> text with <sup>2</sup> mark.</p><p>Body two.</p>
<heading level="1">SUMMARY</heading><p>Summary.</p></description></us-patent-application>"#;
        let d = Document::parse(x).unwrap();
        let b = bib_v4(d.root_element());
        assert_eq!((b.app_number.as_str(), b.filing_date_raw.as_str(), b.pub_date_raw.as_str()), ("12968075", "20101214", "20120614"));
        assert!(b.links.contains("us_app:12000001") && b.links.contains("us_app:12968075"));
        assert_eq!(priority_date(&b, "2010-12-14"), ("2008-01-01".to_owned(), "earliest_related_or_priority_document_date"));
        let (wk, basis) = writer_key(&b).unwrap();
        assert!(wk.starts_with("uspto_first_inventor:") && !wk.contains("doe") && basis == "first_inventor");
        let mut g = BTreeMap::new();
        let items = description_items(child(d.root_element(), "description").unwrap(), &mut g);
        let (head, paras, subs) = background_section(&items).unwrap();
        assert_eq!(head, "BACKGROUND OF THE INVENTION");
        assert_eq!(paras.len(), 4);
        assert_eq!(subs, vec![0]);
        assert_eq!(paras[2], format!("Body one text with {GAP} mark."));
        assert_eq!(g.get("sup"), Some(&1));
    }

    #[test]
    fn pap_document_bib() {
        let x = r#"<patent-application-publication><subdoc-bibliographic-information>
<document-id><doc-number>20030106126</doc-number><kind-code>A1</kind-code><document-date>20030612</document-date></document-id>
<domestic-filing-data><application-number><doc-number>10221222</doc-number></application-number><filing-date>20021230</filing-date></domestic-filing-data>
<foreign-priority-data><priority-application-number><doc-number>1014608</doc-number></priority-application-number><filing-date>20000310</filing-date><country-code>NL</country-code></foreign-priority-data>
<inventors><first-named-inventor><name><given-name>Jan</given-name><family-name>L</family-name></name></first-named-inventor></inventors>
<correspondence-address><name-1>Firm A</name-1><name-2>IP Group</name-2></correspondence-address>
<assignee><organization-name>ACME CO.</organization-name></assignee>
</subdoc-bibliographic-information></patent-application-publication>"#;
        let d = Document::parse(x).unwrap();
        let b = bib_pap(d.root_element());
        assert_eq!(b.app_number, "10221222");
        assert!(b.links.contains("prio:NL:1014608"));
        let (wk, basis) = writer_key(&b).unwrap();
        assert!(wk.starts_with("uspto_practitioner:") && basis == "practitioner_or_correspondent");
        assert!(assignee_key(&b).unwrap().starts_with("uspto_assignee:"));
        assert_eq!(priority_date(&b, "2002-12-30").0, "2000-03-10");
    }
    #[test]
    fn select_joins_families_and_applies_caps() {
        let dir = tempfile::tempdir().unwrap();
        let train = dir.path().join("train");
        let scan = dir.path().join("scan");
        std::fs::create_dir_all(&train).unwrap();
        std::fs::create_dir_all(&scan).unwrap();
        let row = |id: &str, app: &str, links: &[&str], w: &str, text: &str| {
            json!({"id": id, "source_group": format!("uspto_app:{app}"), "writer_key": w, "text": text, "word_count": 160,
                   "provenance": {"family_links": links, "publication_date": "2010-01-07", "assignee_key": null, "writer_key_basis": "first_inventor"}})
        };
        let a = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron";
        let b = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen";
        let rows = [
            row("p1", "11000001", &["us_app:11000001", "us_app:10999999"], "w1", a),
            row("p2", "11000002", &["us_app:11000002", "us_app:10999999"], "w2", b),
            row("p3", "11000003", &["us_app:11000003"], "w1", "red orange yellow green blue indigo violet black white grey pink brown cyan"),
            row("p4", "11000004", &["us_app:11000004", "prio:JP:2008123456"], "w3", "x y z"),
            row("p5", "11000005", &["us_app:11000005"], "w4", &format!("lead {a}")),
            // Own key only as uspto_app:, referenced by a reserved row as us_app:.
            row("p6", "11000006", &["us_pub:20100000006"], "w6", "p six words here"),
            row("p7", "11000007", &["us_app:11000007", "prio:DE:102009000007"], "w7", "p seven words here"),
            row("p8", "11000008", &["us_app:11000008", "prio:GB:0900008", "pct:US09:8"], "w8", "p eight words here"),
            row("p9", "11000009", &["us_app:11000009", "pct:EP09:9"], "w9", "p nine words here"),
        ];
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(train.join("candidates.jsonl"), &body).unwrap();
        let sha = text::sha_hex(body.as_bytes());
        std::fs::write(train.join("report.json"), json!({"schema": crate::adapter::REPORT_SCHEMA, "complete": true, "candidates_sha256": sha, "rejection_reasons": {}}).to_string()).unwrap();
        std::fs::write(scan.join("reserved-metadata.jsonl"), format!("{}\n{}\n", json!({"family_key": "uspto_app:12000009", "family_links": ["prio:JP:2008123456"]}),
            json!({"family_key": "uspto_app:12000010", "family_links": ["us_app:11000006"]}))).unwrap();
        let out = dir.path().join("out");
        let opt = SelectOptions { max_per_writer: 1, max_per_assignee: 2, max_per_year: 10, near_window: 12, seed: "t".into() };
        let rep = select(&train, &[scan], &out, &opt).unwrap();
        let kept: Vec<Value> = io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().map(Result::unwrap).collect();
        let ids: BTreeSet<&str> = kept.iter().map(|r| r["id"].as_str().unwrap()).collect();
        // p4 shares a priority number with a reserved row; p1/p2 are one family; w1 is capped at 1; p5 repeats p1's 12-word window.
        assert!(!ids.contains("p4"));
        assert_eq!(ids.iter().filter(|i| ["p1", "p2"].contains(i)).count(), 1);
        assert!(!(ids.contains("p1") && ids.contains("p3")));
        assert!(!(ids.contains("p1") && ids.contains("p5")));
        assert_eq!(rep["removed_by_reason"]["family_has_reserved_bucket_rows"], 2);
        assert!(!ids.contains("p6"));
        assert!(!ids.contains("p7") && !ids.contains("p9") && ids.contains("p8"));
        assert_eq!(rep["removed_by_reason"]["family_names_non_english_filing_office"], 2);
        let p8 = kept.iter().find(|r| r["id"] == "p8").unwrap();
        assert_eq!(p8["provenance"]["family_filing_offices"], json!(["GB", "US"]));
        for r in &kept {
            assert!(r["source_group"].as_str().unwrap().starts_with("uspto_family:"));
        }
        let fam1 = kept.iter().find(|r| ["p1", "p2"].contains(&r["id"].as_str().unwrap())).unwrap();
        assert_eq!(fam1["source_group"], "uspto_family:us_app:10999999");
        let res: Vec<Value> = io::jsonl_rows(&out.join("reserved-metadata.jsonl")).unwrap().map(Result::unwrap).collect();
        assert_eq!(res[0]["family_key"], "uspto_family:prio:JP:2008123456");
        assert_eq!(res[1]["family_key"], "uspto_family:us_app:11000006");
    }

    #[test]
    fn family_keys_normalise_and_offices_classify() {
        assert_eq!(family_key("uspto_app:10281899"), "us_app:10281899");
        assert_eq!(family_key("us_app:10281899"), "us_app:10281899");
        assert_eq!(family_key("uspto_app:RE12345"), "uspto_app:RE12345");
        assert_eq!(filing_office("prio:JP:2008123456"), Some("JP"));
        assert_eq!(filing_office("pct:EP15:57978"), Some("EP"));
        assert_eq!(filing_office("us_app:10281899"), None);
        assert!(non_english_office("prio:KR:1020080001") && non_english_office("pct:IB08:1"));
        assert!(!non_english_office("prio:GB:0900008") && !non_english_office("pct:US97:5183") && !non_english_office("us_pat:6155150"));
    }

    #[test]
    fn check_rejoins_families_across_key_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let doc = |id: &str, app: &str, links: &[&str], w: &str| {
            json!({"id": id, "collection": COLLECTION, "source_group": format!("uspto_family:us_app:{app}"), "writer_key": w, "word_count": 200,
                   "rights": {"license": LICENSE, "redistribute_text": false}, "extraction": {"unit": {}},
                   "provenance": {"unit_family_key": format!("uspto_app:{app}"), "family_links": links, "filing_date": "2009-01-02",
                                  "publication_date": "2010-01-07", "priority_date": "2008-01-02", "assignee_key": null, "writer_key_basis": "first_inventor"}})
        };
        let docs = dir.path().join("documents.jsonl");
        let res = dir.path().join("reserved.jsonl");
        let write = |rows: &[Value], p: &Path| std::fs::write(p, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
        write(&[json!({"unit_family_key": "uspto_app:13000009", "family_links": ["us_app:13000009"]})], &res);
        write(&[doc("a", "13000001", &["us_pub:20100000001"], "w1"), doc("b", "13000002", &["us_app:13000002", "prio:GB:0900001"], "w2")], &docs);
        assert!(check(&docs, &res, 2, 2).is_ok());
        // b names a as its parent through the printed us_app: prefix.
        write(&[doc("a", "13000001", &["us_pub:20100000001"], "w1"), doc("b", "13000002", &["us_app:13000002", "us_app:13000001"], "w2")], &docs);
        assert!(check(&docs, &res, 2, 2).unwrap_err().to_string().contains("one joined family"));
        write(&[doc("a", "13000001", &["us_app:13000009"], "w1")], &docs);
        assert!(check(&docs, &res, 2, 2).unwrap_err().to_string().contains("reserved"));
        write(&[doc("a", "13000001", &["prio:JP:2008000001"], "w1")], &docs);
        assert!(check(&docs, &res, 2, 2).unwrap_err().to_string().contains("non-English"));
    }
}

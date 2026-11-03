//! Open peer review.
//!
//! - **F1000Research** reviewer reports from PMC Open Access JATS (one
//!   `sub-article` per report). Family and unit: the report. Split group
//!   (`writer_key`): an opaque key of the first listed reviewer, so one
//!   reviewer never spans splits. Report-level dates and licences gate
//!   admission, so a report added to an old article after the cutoff is
//!   never admitted.
//! - **OpenReview reviews from ORB v1.0.1** (Zenodo 8205645, CC BY 4.0).
//!   ORB records no reviewer identity and no review date: the split group is
//!   the reviewed submission, and the date is an upper bound from the venue
//!   year (venues dated 2021 or earlier, and ICLR 2022, whose reviewing ended
//!   in early 2022). SciPost records (no report dates) and undated or later
//!   venues are rejected.
//!
//! Paragraphs repeated verbatim (folded) across many reports are the
//! publisher's questionnaire and confirmation boilerplate; they break
//! paragraph runs instead of becoming an easy "human" signal.
use crate::{
    adapter::Run,
    record::{self, Doc},
    text::{self, Excerpt, Para, Screen},
    xmldoc,
};
use anyhow::{Context, Result, bail};
use roxmltree::Node;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const F1000: &str = "f1000research_reports_historical";
pub const OPENREVIEW: &str = "openreview_orb_reviews_historical";
pub const CAP_DOMAIN: &str = "slop_ninja_review_cap_v1\0";
const XLINK: &str = "http://www.w3.org/1999/xlink";
const GAP_ELEMENTS: &[&str] = &[
    "xref", "inline-formula", "disp-formula", "math", "fig", "table-wrap", "inline-graphic", "graphic", "sup", "sub",
    "fn", "supplementary-material", "media", "list", "disp-quote", "ext-link", "uri", "table",
];
/// Reviewer-report `sub-article` types (PMC uses `peer-review` for most
/// F1000Research reports); author `response` sub-articles are never read.
const REPORT_TYPES: &[&str] = &["peer-review", "reviewer-report", "ref-report", "referee-report"];

fn child<'a, 'b>(n: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    n.children().find(|c| c.has_tag_name(name))
}

/// `pub-date` children as `YYYY-MM-DD` (missing day or month: 01).
pub fn jats_date(n: Node) -> Option<String> {
    let num = |k: &str| child(n, k).and_then(|c| c.text()).and_then(|t| t.trim().parse::<u32>().ok());
    let y = num("year")?;
    Some(format!("{y:04}-{:02}-{:02}", num("month").unwrap_or(1), num("day").unwrap_or(1)))
}

/// Opaque reviewer key: never the name itself.
pub fn reviewer_key(source: &str, name: &str) -> Option<String> {
    let f = text::folded_tokens(name).join(" ");
    (!f.is_empty()).then(|| format!("{source}_reviewer:{}", &text::sha_hex(format!("slop_ninja_reviewer_v1\0{f}").as_bytes())[..20]))
}

/// One review or report before screening.
pub struct Report {
    pub unit: String,
    /// Split group; `None` is counted and skipped.
    pub group: Option<String>,
    pub date: Option<String>,
    pub date_basis: &'static str,
    /// `Err(reason)` rejects the report before rendering.
    pub gate: Result<(), &'static str>,
    pub licence: Option<String>,
    pub licence_basis: &'static str,
    pub paras: Vec<Para>,
    pub url: String,
    pub attribution: String,
    pub unit_v: Value,
    pub provenance: Value,
}

pub struct SourceCfg {
    pub collection: &'static str,
    pub id_prefix: &'static str,
    pub evidence_url: &'static str,
    pub notes: &'static str,
    pub method: String,
    pub grouping: &'static str,
    pub limits: Vec<&'static str>,
}

pub struct Options<'a> {
    pub corpus: &'a str,
    pub max_per_group: usize,
    /// A folded paragraph seen in at least this many distinct reports is boilerplate.
    pub min_template_reports: usize,
}

struct Held {
    rank: u64,
    unit: String,
    group: String,
    excerpt: Excerpt,
    date: String,
    date_basis: &'static str,
    rights: Value,
    unit_v: Value,
    provenance: Value,
}

/// Admit one excerpt per report, after boilerplate marking and group caps.
pub fn finish(reports: Vec<Report>, o: &Options, run: &mut Run, cfg: &SourceCfg) -> Result<Value> {
    let mut freq: HashMap<u64, HashSet<usize>> = HashMap::new();
    for (i, r) in reports.iter().enumerate() {
        for p in &r.paras {
            let f = text::folded(&p.text);
            if !f.is_empty() {
                freq.entry(text::h64(f.as_bytes())).or_default().insert(i);
            }
        }
    }
    let template: HashSet<u64> = freq.into_iter().filter(|(_, s)| s.len() >= o.min_template_reports).map(|(k, _)| k).collect();
    let mut held: BTreeMap<String, Vec<Held>> = BTreeMap::new();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut weak: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut template_paras = 0u64;
    for mut r in reports {
        run.units_seen += 1;
        if !seen.insert(r.unit.clone()) {
            run.count("same_report_in_another_record");
            continue;
        }
        let Some(group) = r.group.clone() else {
            run.count("report_without_group_key");
            continue;
        };
        if !run.sampled_meta_grouped(&group, &r.unit, || json!({"unit_id": r.unit, "date": r.date, "url": r.url}))? {
            continue;
        }
        if let Err(x) = r.gate {
            run.reject(&r.unit, x)?;
            continue;
        }
        let Some(date) = r.date.clone() else {
            run.reject(&r.unit, "report_without_date")?;
            continue;
        };
        if !record::before_cutoff(&date) {
            run.reject(&r.unit, "report_not_before_2022_11_30")?;
            continue;
        }
        let licence = match r.licence.clone() {
            Some(l) if record::admitted_license(&l) => l,
            Some(_) => {
                run.reject(&r.unit, "report_licence_not_admitted")?;
                continue;
            }
            None => {
                run.reject(&r.unit, "report_without_licence")?;
                continue;
            }
        };
        for p in r.paras.iter_mut() {
            if template.contains(&text::h64(text::folded(&p.text).as_bytes())) {
                *p = Para::non_prose(std::mem::take(&mut p.text), "repeated_template_paragraph");
                template_paras += 1;
            }
            if let Some(x) = p.reason {
                *breaks.entry(x).or_default() += 1;
            }
        }
        let Some(ex) = text::paragraph_run(&r.paras) else {
            run.reject(&r.unit, "no_paragraph_run_within_bounds")?;
            continue;
        };
        if let Some(x) = text::english_screen(&ex.text).or_else(|| text::strong_transcription_artifact(&ex.text)) {
            run.reject(&r.unit, x)?;
            continue;
        }
        let flags = text::transcription_artifacts(&ex.text);
        for f in &flags {
            *weak.entry(f).or_default() += 1;
        }
        *by_year.entry(date[..4].to_owned()).or_default() += 1;
        let mut rights = record::rights(&licence, r.attribution.clone(), cfg.evidence_url, cfg.notes);
        rights["licence_basis"] = json!(r.licence_basis);
        rights["redistribute_text"] = json!(false);
        rights["admission_decision"] = json!("default_2026-10-27: CC BY peer review text; kept in ignored storage and not redistributed by this pool");
        let mut unit_v = r.unit_v.clone();
        unit_v["transcription_artifacts"] = json!(flags);
        unit_v["transcription_artifact_method"] = json!(text::TRANSCRIPTION_ARTIFACT_METHOD);
        let mut provenance = r.provenance.clone();
        provenance["source_url"] = json!(r.url);
        provenance["corpus"] = json!(o.corpus);
        provenance["bucket_key"] = json!(group);
        provenance["bucket"] = json!(crate::adapter::bucket(&group));
        let h = Held {
            rank: text::h64(format!("{CAP_DOMAIN}{}", r.unit).as_bytes()),
            unit: r.unit.clone(),
            group: group.clone(),
            excerpt: ex,
            date,
            date_basis: r.date_basis,
            rights,
            unit_v,
            provenance,
        };
        let v = held.entry(group).or_default();
        v.push(h);
        if v.len() > o.max_per_group {
            v.sort_by_key(|x| x.rank);
            let d = v.pop().expect("over cap");
            run.reject(&d.unit, "group_cap_not_selected")?;
        }
    }
    let groups = held.len();
    for (_, mut v) in held {
        v.sort_by_key(|x| x.rank);
        for h in v {
            let doc = Doc {
                id: format!("pool-{}-{}", cfg.id_prefix, &text::sha_hex(h.unit.as_bytes())[..16]),
                collection: cfg.collection,
                domain: "peer_review",
                source_group: h.unit.clone(),
                writer_key: Some(h.group),
                evidence: "historical_proxy",
                excerpt: &h.excerpt,
                extraction_method: &cfg.method,
                unit: h.unit_v,
                provenance: h.provenance,
                date: Some(h.date),
                date_basis: h.date_basis,
                cutoff_policy: "dated_before_cutoff",
                rights: h.rights,
                aliases: vec![],
            };
            run.accept(&h.unit, &doc)?;
        }
    }
    Ok(json!({"groups_with_candidates": groups, "max_per_group": o.max_per_group, "template_paragraph_min_reports": o.min_template_reports,
              "distinct_template_paragraphs": template.len(), "template_paragraph_occurrences_marked": template_paras,
              "paragraph_breaks": breaks, "candidates_by_year_before_caps": by_year,
              "recorded_transcription_artifacts_in_candidates": weak, "grouping": cfg.grouping, "limits": cfg.limits}))
}

fn f1000_reports(doc: &roxmltree::Document, file: &str) -> Vec<Report> {
    let root = doc.root_element();
    let am = root.descendants().find(|n| n.has_tag_name("article-meta"));
    let id_of = |kinds: &[&str]| {
        am.and_then(|m| m.children().find(|c| c.has_tag_name("article-id") && c.attribute("pub-id-type").is_some_and(|t| kinds.contains(&t))))
            .and_then(|c| c.text())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let article_doi = id_of(&["doi"]);
    let lic_of = |l: Node| {
        let mut urls: Vec<String> = l.attribute((XLINK, "href")).into_iter().map(str::to_owned).collect();
        urls.extend(l.descendants().filter(|c| c.has_tag_name("license_ref")).filter_map(|c| c.text()).map(str::to_owned));
        urls.extend(l.descendants().filter(|c| c.has_tag_name("ext-link")).filter_map(|c| c.attribute((XLINK, "href"))).map(str::to_owned));
        urls.iter().find_map(|u| record::license_from_url(u))
    };
    let article_licence = am.and_then(|m| child(m, "permissions")).and_then(|p| p.descendants().find(|c| c.has_tag_name("license"))).and_then(lic_of);
    let pmcid = id_of(&["pmc", "pmcid"]);
    let mut out = Vec::new();
    for sa in root.descendants().filter(|n| n.has_tag_name("sub-article")) {
        if !sa.attribute("article-type").is_some_and(|t| REPORT_TYPES.contains(&t)) {
            continue;
        }
        let fs = child(sa, "front-stub").or_else(|| child(sa, "front"));
        let report_doi = fs
            .and_then(|f| f.children().find(|c| c.has_tag_name("article-id") && c.attribute("pub-id-type") == Some("doi")))
            .and_then(|c| c.text())
            .unwrap_or("")
            .trim()
            .to_owned();
        let report_id = sa.attribute("id").unwrap_or("").to_owned();
        let date = fs.and_then(|f| f.children().find(|c| c.has_tag_name("pub-date"))).and_then(jats_date);
        let own = fs.and_then(|f| f.descendants().find(|c| c.has_tag_name("license"))).and_then(lic_of);
        // Reports published before report-level licence links carry the
        // publisher's policy licence ("peer review reports ... are also
        // available under the CC BY license"), accepted only when the
        // article itself links a CC BY licence.
        let (licence, licence_basis) = match (own, &article_licence) {
            (Some(l), _) => (Some(l), "report_licence_link"),
            (None, Some(a)) if a.starts_with("CC-BY-") && !a.starts_with("CC-BY-NC") && !a.starts_with("CC-BY-ND") && !a.starts_with("CC-BY-SA") => {
                (Some(a.clone()), "f1000research_policy_reports_cc_by_and_article_cc_by_link")
            }
            (None, _) => (None, "none"),
        };
        let reviewers: Vec<String> = fs
            .into_iter()
            .flat_map(|f| f.descendants().filter(|c| c.has_tag_name("contrib")))
            .filter_map(|c| child(c, "name"))
            .map(|n| {
                let g = child(n, "given-names").and_then(|x| x.text()).unwrap_or("");
                let s = child(n, "surname").and_then(|x| x.text()).unwrap_or("");
                format!("{} {}", g.trim(), s.trim()).trim().to_owned()
            })
            .filter(|n| !n.is_empty())
            .collect();
        let recommendation = fs
            .and_then(|f| f.descendants().find(|c| c.has_tag_name("custom-meta") && child(*c, "meta-name").and_then(|m| m.text()) == Some("recommendation")))
            .and_then(|c| child(c, "meta-value"))
            .and_then(|v| v.text())
            .map(str::to_owned);
        let version_label = fs.and_then(|f| f.descendants().find(|c| c.has_tag_name("related-article"))).map(xmldoc::text_of);
        let mut paras = Vec::new();
        if let Some(body) = child(sa, "body") {
            for n in body.descendants().filter(|n| n.has_tag_name("p") || n.has_tag_name("title")) {
                // Paragraphs nested in lists or other paragraphs belong to their container.
                if n.ancestors().skip(1).take_while(|a| *a != body).any(|a| ["list", "p", "disp-quote", "table-wrap"].iter().any(|t| a.has_tag_name(*t))) {
                    continue;
                }
                if n.has_tag_name("title") {
                    paras.push(Para::non_prose(xmldoc::text_of(n), "heading"));
                } else {
                    paras.push(xmldoc::paragraph(n, GAP_ELEMENTS, &[], Screen::XML));
                }
            }
        }
        let unit = format!("f1000:report:{}", if report_doi.is_empty() { format!("{article_doi}#{report_id}") } else { report_doi.clone() });
        let url = format!("https://doi.org/{}", if report_doi.is_empty() { &article_doi } else { &report_doi });
        out.push(Report {
            group: reviewers.first().and_then(|n| reviewer_key("f1000", n)),
            date,
            date_basis: "report_publication_date",
            gate: Ok(()),
            attribution: format!("Peer review report by {}, {url}, on {article_doi}; F1000Research; paragraphs extracted, markup omitted", reviewers.join(", ")),
            licence,
            licence_basis,
            paras,
            unit_v: json!({"kind": "jats_reviewer_report_sub_article", "report_id": report_id, "file": file, "excerpt_unit": "whole paragraphs of one reviewer report"}),
            provenance: json!({"report_doi": report_doi, "article_doi": article_doi, "pmcid": pmcid,
                               "access": "PMC Open Access dataset (pmc-oa-opendata), JATS XML",
                               "recommendation": recommendation, "reviewed_version": version_label, "reviewer_count": reviewers.len()}),
            url,
            unit,
        });
    }
    out
}

/// F1000Research reports from PMC JATS files (`Source`: a directory or zip).
pub fn f1000(source: &xmldoc::Source, o: &Options, run: &mut Run) -> Result<Value> {
    let mut reports = Vec::new();
    let (mut files, mut unreadable, mut not_f1000) = (0u64, 0u64, 0u64);
    source.each(64 * 1024 * 1024, |name, bytes| {
        files += 1;
        let Some(b) = bytes else {
            unreadable += 1;
            return Ok(true);
        };
        let s = String::from_utf8_lossy(b);
        let prepared = xmldoc::prepare(&s);
        let Ok(doc) = xmldoc::document(&prepared) else {
            unreadable += 1;
            return Ok(true);
        };
        let journal = doc.descendants().find(|n| n.has_tag_name("journal-title")).map(xmldoc::text_of).unwrap_or_default();
        if !journal.to_lowercase().starts_with("f1000") {
            not_f1000 += 1;
            return Ok(true);
        }
        reports.extend(f1000_reports(&doc, name));
        Ok(true)
    })?;
    let cfg = SourceCfg {
        collection: F1000,
        id_prefix: "f1000",
        evidence_url: "https://f1000research.com/about/policies",
        notes: "Open peer review report published under its own CC BY licence; reviewer names are public on the report. Questionnaire and confirmation boilerplate break paragraph runs.",
        method: format!("jats_sub_article_body_paragraphs;repeated_template_paragraphs_break;{}", text::PARAGRAPH_RUN_METHOD),
        grouping: "family and unit: one report; split group and writer_key: opaque hash of the first listed reviewer's folded name",
        limits: vec!["Co-reviewed reports are grouped by their first reviewer only.", "Name variants of one reviewer are not merged.",
                     "Reviewers are often non-native academic writers; the human label is a pre-cutoff proxy."],
    };
    let mut extra = finish(reports, o, run, &cfg)?;
    extra["files"] = json!(files);
    extra["unreadable_files"] = json!(unreadable);
    extra["files_not_f1000research"] = json!(not_f1000);
    Ok(extra)
}

// ---------------------------------------------------------------------------
// Python-literal reader for the ORB export (dicts, lists, tuples, sets,
// strings, numbers, True/False/None and `Name(key=value)` object reprs).

pub struct PyLit<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

impl<'a> PyLit<'a> {
    pub fn parse(src: &'a str) -> Result<Value> {
        let mut p = PyLit { s: src.as_bytes(), src, i: 0 };
        let v = p.value()?;
        p.ws();
        anyhow::ensure!(p.i == p.s.len(), "trailing data at byte {}", p.i);
        Ok(v)
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> Result<()> {
        anyhow::ensure!(self.peek() == Some(c), "expected '{}' at byte {}", c as char, self.i);
        self.i += 1;
        Ok(())
    }
    fn seq(&mut self, close: u8) -> Result<Vec<Value>> {
        let mut v = Vec::new();
        loop {
            if self.peek() == Some(close) {
                self.i += 1;
                return Ok(v);
            }
            v.push(self.value()?);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(c) if c == close => {}
                _ => bail!("expected ',' or '{}' at byte {}", close as char, self.i),
            }
        }
    }
    fn key_string(v: Value) -> String {
        match v {
            Value::String(s) => s,
            other => other.to_string(),
        }
    }
    fn value(&mut self) -> Result<Value> {
        match self.peek().context("unexpected end")? {
            b'{' => {
                self.i += 1;
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(json!({}));
                }
                let first = self.value()?;
                if self.peek() == Some(b':') {
                    self.i += 1;
                    let mut m = Map::new();
                    let v = self.value()?;
                    m.insert(Self::key_string(first), v);
                    loop {
                        match self.peek() {
                            Some(b',') => {
                                self.i += 1;
                                if self.peek() == Some(b'}') {
                                    self.i += 1;
                                    return Ok(Value::Object(m));
                                }
                                let k = self.value()?;
                                self.eat(b':')?;
                                let v = self.value()?;
                                m.insert(Self::key_string(k), v);
                            }
                            Some(b'}') => {
                                self.i += 1;
                                return Ok(Value::Object(m));
                            }
                            _ => bail!("bad dict at byte {}", self.i),
                        }
                    }
                }
                // A set literal.
                let mut v = vec![first];
                if self.peek() == Some(b',') {
                    self.i += 1;
                    v.extend(self.seq(b'}')?);
                } else {
                    self.eat(b'}')?;
                }
                Ok(Value::Array(v))
            }
            b'[' => {
                self.i += 1;
                Ok(Value::Array(self.seq(b']')?))
            }
            b'(' => {
                self.i += 1;
                Ok(Value::Array(self.seq(b')')?))
            }
            b'\'' | b'"' => self.string(),
            c if c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                self.i += 1;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || matches!(self.s[self.i], b'.' | b'+' | b'-' | b'_')) {
                    self.i += 1;
                }
                let t = &self.src[st..self.i];
                if let Ok(n) = t.parse::<i64>() {
                    return Ok(json!(n));
                }
                Ok(t.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or_else(|| json!(t)))
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let st = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || matches!(self.s[self.i], b'_' | b'.')) {
                    self.i += 1;
                }
                let name = &self.src[st..self.i];
                match name {
                    "True" => return Ok(json!(true)),
                    "False" => return Ok(json!(false)),
                    "None" => return Ok(Value::Null),
                    "nan" | "inf" => return Ok(json!(name)),
                    _ => {}
                }
                self.eat(b'(')?;
                let mut m = Map::new();
                m.insert("__type__".into(), json!(name));
                let mut args = Vec::new();
                loop {
                    if self.peek() == Some(b')') {
                        self.i += 1;
                        break;
                    }
                    // keyword argument?
                    let save = self.i;
                    let st = self.i;
                    while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                        self.i += 1;
                    }
                    let kw = &self.src[st..self.i];
                    if !kw.is_empty() && self.peek() == Some(b'=') {
                        self.i += 1;
                        let v = self.value()?;
                        m.insert(kw.to_owned(), v);
                    } else {
                        self.i = save;
                        args.push(self.value()?);
                    }
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b')') => {}
                        _ => bail!("bad call at byte {}", self.i),
                    }
                }
                m.insert("__args__".into(), Value::Array(args));
                Ok(Value::Object(m))
            }
            c => bail!("unexpected '{}' at byte {}", c as char, self.i),
        }
    }
    fn hex(&mut self, n: usize) -> Result<char> {
        let h = self.src.get(self.i..self.i + n).context("short escape")?;
        self.i += n;
        char::from_u32(u32::from_str_radix(h, 16)?).context("bad code point")
    }
    fn string(&mut self) -> Result<Value> {
        let q = self.s[self.i];
        self.i += 1;
        let mut out = String::new();
        loop {
            let st = self.i;
            while self.i < self.s.len() && self.s[self.i] != q && self.s[self.i] != b'\\' {
                self.i += 1;
            }
            out.push_str(&self.src[st..self.i]);
            match self.s.get(self.i) {
                None => bail!("unterminated string"),
                Some(&c) if c == q => {
                    self.i += 1;
                    return Ok(Value::String(out));
                }
                _ => {
                    let e = *self.s.get(self.i + 1).context("bad escape")?;
                    self.i += 2;
                    match e {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'\\' => out.push('\\'),
                        b'\'' => out.push('\''),
                        b'"' => out.push('"'),
                        b'0' => out.push('\0'),
                        b'a' => out.push('\u{7}'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'v' => out.push('\u{b}'),
                        b'x' => out.push(self.hex(2)?),
                        b'u' => out.push(self.hex(4)?),
                        b'U' => out.push(self.hex(8)?),
                        b'\n' => {}
                        other => bail!("unsupported escape \\{} at byte {}", other as char, self.i),
                    }
                }
            }
        }
    }
}

const LIST_MARKERS: &[&str] = &["- ", "* ", "• ", "+ ", "· ", "– ", "— ", "> "];

fn list_line(l: &str) -> bool {
    let t = l.trim_start();
    if LIST_MARKERS.iter().any(|m| t.starts_with(m)) {
        return true;
    }
    // 1. 1) (1) [1] a) i) A.
    let t2 = t.trim_start_matches(['(', '[']);
    let n = t2.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &t2[n..];
    if n > 0 && n <= 3 && rest.starts_with(['.', ')', ']', ':']) {
        return true;
    }
    let mut cs = t.chars();
    matches!((cs.next(), cs.next(), cs.next()), (Some(a), Some(')'), Some(' ')) if a.is_ascii_alphabetic())
}

fn markup_line(l: &str) -> bool {
    ["$", "\\", "**", "##", "`", "|", "http", "[[", "]("].iter().any(|m| l.contains(m))
}

/// Plain-text review into paragraphs: blank lines and single line breaks
/// separate paragraphs, except in hard-wrapped blocks (most lines end
/// mid-sentence), which are joined. List items, markdown, LaTeX and URLs
/// break runs.
pub fn plain_paras(s: &str) -> Vec<Para> {
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    for block in s.split("\n\n").flat_map(|b| b.split("\n \n")) {
        let lines: Vec<&str> = block.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            continue;
        }
        let inner = &lines[..lines.len() - 1];
        let wrapped = lines.len() >= 3
            && inner.iter().filter(|l| !l.trim_end().ends_with(['.', '!', '?', ':', ';']) && l.len() >= 40).count() * 10 >= inner.len() * 6
            && !lines.iter().any(|l| list_line(l));
        let units: Vec<String> = if wrapped { vec![lines.join(" ")] } else { lines.iter().map(|l| l.to_string()).collect() };
        for u in units {
            if list_line(&u) {
                out.push(Para::non_prose(u, "list_or_indent"));
            } else if markup_line(&u) {
                out.push(Para::non_prose(u, "markdown_latex_or_url"));
            } else {
                out.push(Para::classify(text::squash(&u), Screen::MARKUP));
            }
        }
    }
    out
}

/// Upper-bound date for an OpenReview venue year, or the rejection reason.
pub fn openreview_date(venue_name: &str, year: Option<i64>) -> Result<String, &'static str> {
    let Some(y) = year else { return Err("venue_without_year") };
    if y <= 2021 {
        return Ok(format!("{y:04}-12-31"));
    }
    if y == 2022 && venue_name.starts_with("ICLR.cc/2022/Conference") {
        return Ok("2022-04-30".to_owned());
    }
    Err(if y >= 2023 { "venue_year_2023_or_later" } else { "venue_year_2022_reviews_may_follow_cutoff" })
}

fn s(v: &Value) -> String {
    match v {
        Value::String(x) => x.clone(),
        Value::Null => String::new(),
        o => o.to_string(),
    }
}

/// OpenReview reviews from the ORB v1.0.1 export (a Python literal).
pub fn orb(path: &std::path::Path, o: &Options, run: &mut Run) -> Result<Value> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let v = PyLit::parse(&raw)?;
    drop(raw);
    let subs = v["orb_submissions"].as_array().context("orb_submissions")?;
    let mut reports = Vec::new();
    let mut by_venue: BTreeMap<String, u64> = BTreeMap::new();
    let mut scipost = 0u64;
    for sub in subs {
        let sid = s(&sub["id"]);
        let venue = &sub["venue"];
        let vname = s(&venue["name"]);
        let year = match &venue["year"] {
            Value::String(y) => y.trim().parse::<i64>().ok(),
            Value::Number(n) => n.as_i64(),
            _ => None,
        };
        let pdf = sub["article_versions"].as_object().into_iter().flat_map(|m| m.values()).find_map(|x| x["pdf"].as_str().map(str::to_owned)).unwrap_or_default();
        let is_openreview = !sid.starts_with("scipost") && (pdf.contains("openreview.net") || s(&venue["id"]).contains('/'));
        let mut versions: Vec<(&String, &Value)> = sub["article_versions"].as_object().into_iter().flat_map(|m| m.iter()).collect();
        versions.sort_by_key(|(k, _)| k.parse::<i64>().unwrap_or(0));
        for (ver, av) in versions {
            for (k, r) in av["reviews"].as_array().into_iter().flatten().enumerate() {
                let rid = s(&r["id"]);
                let unit = if rid.is_empty() { format!("orb:{sid}:v{ver}:r{k}") } else { format!("openreview:note:{rid}") };
                *by_venue.entry(format!("{vname}|{}", year.map(|y| y.to_string()).unwrap_or_default())).or_default() += 1;
                let gate = if !is_openreview {
                    scipost += 1;
                    Err("not_openreview_record_undated_report")
                } else if rid.is_empty() {
                    Err("review_without_note_id")
                } else {
                    Ok(())
                };
                let date = openreview_date(&vname, year);
                let gate = gate.and(date.as_ref().map(|_| ()).map_err(|e| *e));
                let url = if rid.is_empty() { pdf.clone() } else { format!("https://openreview.net/forum?id={sid}&noteId={rid}") };
                let body = s(&r["review"]);
                reports.push(Report {
                    unit,
                    group: Some(format!("openreview:forum:{sid}")),
                    date: date.ok(),
                    date_basis: "upper_bound_from_venue_year_reviews_precede_it",
                    gate,
                    licence: Some("CC-BY-4.0".into()),
                    licence_basis: "openreview_terms_comments_cc_by_4_0_and_orb_dataset_cc_by_4_0",
                    paras: plain_paras(&body),
                    attribution: format!("Anonymous OpenReview reviewer, {url}; review text from the ORB dataset (Szumega et al., Zenodo 10.5281/zenodo.8205645, CC BY 4.0); OpenReview comments CC BY 4.0"),
                    unit_v: json!({"kind": "openreview_review_text", "note_id": rid, "article_version": ver, "review_text_sha256": text::sha_hex(body.as_bytes()),
                                   "excerpt_unit": "whole paragraphs of one review"}),
                    provenance: json!({"forum_id": sid, "note_id": rid, "venue": vname, "venue_id": s(&venue["id"]), "venue_year": year,
                                       "decision": s(&sub["decision"]), "dataset": "ORB v1.0.1 (Zenodo record 8205645, 2023-08-01_OrbDataset_mainfile.json)"}),
                    url,
                });
            }
        }
    }
    let cfg = SourceCfg {
        collection: OPENREVIEW,
        id_prefix: "openreview",
        evidence_url: "https://openreview.net/legal/terms",
        notes: "OpenReview comments are released under CC BY 4.0 by the OpenReview terms of use; ORB redistributes them under CC BY 4.0. Reviewers are anonymous; no reviewer identity is recorded.",
        method: format!("orb_plain_text_review;line_and_blank_line_paragraphs;hard_wrap_join;lists_markdown_latex_break;repeated_template_paragraphs_break;{}", text::PARAGRAPH_RUN_METHOD),
        grouping: "family and unit: one review; split group and writer_key: the reviewed submission (OpenReview forum); ORB has no reviewer identity",
        limits: vec!["One anonymous reviewer may review submissions in different splits (no reviewer identity in ORB).",
                     "Dates are upper bounds from the venue year, not review timestamps.",
                     "ORB v1.0.1 lacks ICLR 2017 and ICLR 2022 main-conference reviews; the direct OpenReview API requires a browser challenge and was not used.",
                     "Reviewers are often non-native academic writers; the human label is a pre-cutoff proxy."],
    };
    let mut extra = finish(reports, o, run, &cfg)?;
    extra["submissions"] = json!(subs.len());
    extra["reviews_by_venue_year"] = json!(by_venue);
    extra["non_openreview_reviews"] = json!(scipost);
    Ok(extra)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn prose(n: usize, seed: &str) -> String {
        let base = ["the", "authors", "should", "explain", "how", "the", "sample", "was", "chosen", "and", "why", "it", "is", "small", "in", seed];
        let w: Vec<&str> = (0..n - 1).map(|i| base[i % base.len()]).collect();
        format!("{} end.", w.join(" "))
    }
    fn report(id: &str, date: (u32, u32, u32), name: (&str, &str), paras: &[String], lic: &str) -> String {
        let ps: String = paras.iter().map(|p| format!("<p>{p}</p>")).collect();
        let kind = if id.ends_with('1') || id.ends_with('3') { "peer-review" } else if id.starts_with("resp") { "response" } else { "reviewer-report" };
        format!("<sub-article article-type=\"{kind}\" id=\"{id}\"><front-stub><article-id pub-id-type=\"doi\">10.5256/x.{id}</article-id>\
<contrib-group><contrib contrib-type=\"author\"><name><surname>{}</surname><given-names>{}</given-names></name><role>Referee</role></contrib></contrib-group>\
<pub-date pub-type=\"epub\"><day>{}</day><month>{}</month><year>{}</year></pub-date><permissions><license><license_ref>{lic}</license_ref><license-p>x</license-p></license></permissions>\
</front-stub><body>{ps}</body></sub-article>", name.1, name.0, date.2, date.1, date.0)
    }
    fn article(doi: &str, subs: &str) -> String {
        format!("<?xml version=\"1.0\"?><article xmlns:xlink=\"http://www.w3.org/1999/xlink\"><front><journal-meta><journal-title-group><journal-title>F1000Research</journal-title></journal-title-group></journal-meta>\
<article-meta><article-id pub-id-type=\"doi\">{doi}</article-id><permissions><license xlink:href=\"https://creativecommons.org/licenses/by/4.0/\"><license-p>x</license-p></license></permissions></article-meta></front><body><p>Article body.</p></body>{subs}</article>")
    }

    #[test]
    fn f1000_reports_are_dated_licensed_grouped_and_template_screened() {
        let cc = "https://creativecommons.org/licenses/by/4.0/";
        let q = "Is the work clearly and accurately presented and does it cite the current literature?".to_owned();
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("xml");
        std::fs::create_dir(&src).unwrap();
        let mut subs = String::new();
        for i in 0..5 {
            subs.push_str(&report(&format!("r{i}"), (2019, 3, 1), ("Ann", &format!("Reviewer{i}")), &[prose(120, &format!("a{i}")), prose(110, &format!("b{i}")), q.clone(), "Yes".into()], cc));
        }
        subs.push_str(&report("late", (2023, 1, 5), ("Bob", "Late"), &[prose(240, "c")], cc));
        subs.push_str(&report("resp0", (2019, 4, 1), ("Ed", "Author"), &[prose(240, "g")], cc));
        subs.push_str(&report("nc", (2019, 1, 5), ("Cy", "Nc"), &[prose(240, "d")], "https://creativecommons.org/licenses/by-nc/4.0/"));
        // No report-level licence: the article's CC BY link and publisher policy apply.
        subs.push_str(&report("old", (2016, 2, 1), ("Fay", "Old"), &[prose(240, "h")], "").replace("<permissions><license><license_ref></license_ref><license-p>x</license-p></license></permissions>", ""));
        // The template question would otherwise extend this short run past the bound.
        subs.push_str(&report("short", (2019, 1, 5), ("Di", "Short"), &[prose(140, "e"), q.clone()], cc));
        std::fs::write(src.join("a.xml"), article("10.12688/f1000research.1.1", &subs)).unwrap();
        // The same report again in a later version's record.
        std::fs::write(src.join("b.xml"), article("10.12688/f1000research.1.2", &report("r0", (2019, 3, 1), ("Ann", "Reviewer0"), &[prose(240, "f")], cc))).unwrap();
        let out = d.path().join("o");
        let mut run = Run::start(&out, "f1000", F1000, json!({}), None).unwrap();
        let source = xmldoc::Source::open(&src).unwrap();
        let extra = f1000(&source, &Options { corpus: "synthetic", max_per_group: 3, min_template_reports: 5 }, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["candidates"], 6, "{rep}");
        assert_eq!(rep["rejection_reasons"]["report_not_before_2022_11_30"], 1);
        assert_eq!(rep["rejection_reasons"]["report_licence_not_admitted"], 1);
        assert_eq!(rep["rejection_reasons"]["no_paragraph_run_within_bounds"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["same_report_in_another_record"], 1);
        assert_eq!(rep["extra"]["distinct_template_paragraphs"], 2, "the question and its repeated answer");
        let rows = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<Vec<_>>>().unwrap();
        assert!(rows.iter().filter(|r| r["date"]["value"] != "2016-02-01").all(|r| r["word_count"] == 230));
        let old = rows.iter().find(|r| r["date"]["value"] == "2016-02-01").unwrap();
        assert_eq!(old["rights"]["licence_basis"], "f1000research_policy_reports_cc_by_and_article_cc_by_link");
        assert_eq!(old["rights"]["license"], "CC-BY-4.0");
        let w = rows[0]["writer_key"].as_str().unwrap();
        assert!(w.starts_with("f1000_reviewer:") && !w.to_lowercase().contains("reviewer0") && !w.contains("ann"));
        assert_eq!(rows[0]["rights"]["license"], "CC-BY-4.0");
        assert_eq!(rows[0]["date"]["value"], "2019-03-01");
    }

    #[test]
    fn python_literal_reader() {
        let v = PyLit::parse("{'a': [1, -2.5, True, None], \"b\": 'it\\'s\\n\\u00e9', 0: {Obj(score=2, description='x'), Obj(3)}, 'e': {}, 't': (1,)}").unwrap();
        assert_eq!(v["a"], json!([1, -2.5, true, null]));
        assert_eq!(v["b"], "it's\né");
        assert_eq!(v["0"][0]["score"], 2);
        assert_eq!(v["0"][1]["__args__"], json!([3]));
        assert_eq!(v["e"], json!({}));
        assert_eq!(v["t"], json!([1]));
        assert!(PyLit::parse("{'a': 1").is_err());
    }

    #[test]
    fn plain_text_paragraphs_lists_and_wraps() {
        let p = plain_paras("Summary:\nThe paper studies a thing and it does so with care and detail.\n- a bullet item that is long enough here\n\n$x^2$ is shown in the paper here today.");
        let r: Vec<_> = p.iter().map(|x| x.reason).collect();
        assert_eq!(r, vec![Some("short_paragraph"), None, Some("list_or_indent"), Some("markdown_latex_or_url")]);
        let wrapped = "The paper proposes a method for learning representations\nfrom unlabelled data and shows that the method works well\nacross a range of tasks that the authors have chosen with\ncare and attention to detail.";
        let p = plain_paras(wrapped);
        assert_eq!(p.len(), 1);
        assert!(p[0].prose, "{p:?}");
    }

    #[test]
    fn openreview_dates_and_orb_adapter() {
        assert_eq!(openreview_date("ICLR.cc/2019/Conference/", Some(2019)).unwrap(), "2019-12-31");
        assert_eq!(openreview_date("ICLR.cc/2022/Conference/", Some(2022)).unwrap(), "2022-04-30");
        assert!(openreview_date("NeurIPS.cc/2022/Workshop/X/", Some(2022)).is_err());
        assert_eq!(openreview_date("X/2023/", Some(2023)).unwrap_err(), "venue_year_2023_or_later");
        let rev = |id: &str, t: &str| format!("{{'id': '{id}', 'summary': 's', 'review': '{t}', 'rating': {{'r': OrbGrading(score=2, description=\"x\")}}}}");
        let body = format!("Summary\\n{}\\n\\n{}", prose(120, "a"), prose(110, "b"));
        let sub = |sid: &str, venue: &str, year: &str, revs: &str, pdf: &str| format!("{{'id': '{sid}', 'article_versions': {{0: {{'title': 't', 'pdf': '{pdf}', 'reviews': [{revs}]}}}}, 'decision': 'Accept', 'decision_flag': False, 'venue': {{'id': '{venue}-/Blind_Submission', 'name': '{venue}', 'year': {year}}}}}");
        let lit = format!("{{'orb_submissions': [{}, {}, {}]}}",
            sub("f1", "ICLR.cc/2019/Conference/", "'2019'", &format!("{}, {}", rev("n1", &body), rev("n2", &body.replace("authors", "writers"))), "https://openreview.net//pdf/x.pdf"),
            sub("f2", "ICLR.cc/2023/Conference/", "'2023'", &rev("n3", &body.replace("authors", "people")), "https://openreview.net//pdf/y.pdf"),
            sub("scipost_202001_1", "SciPost Physics", "None", &format!("{{'id': None, 'summary': None, 'review': '{}', 'rating': {{}}}}", body.replace("authors", "folk")), "https://scipost.org/x"));
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("orb.json");
        std::fs::write(&f, lit).unwrap();
        let out = d.path().join("o");
        let mut run = Run::start(&out, "orb", OPENREVIEW, json!({}), None).unwrap();
        let extra = orb(&f, &Options { corpus: "synthetic", max_per_group: 5, min_template_reports: 5 }, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["candidates"], 2, "{rep}");
        assert_eq!(rep["rejection_reasons"]["venue_year_2023_or_later"], 1);
        assert_eq!(rep["rejection_reasons"]["not_openreview_record_undated_report"], 1);
        let rows = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<Vec<_>>>().unwrap();
        assert!(rows.iter().all(|r| r["writer_key"] == "openreview:forum:f1" && r["word_count"] == 230 && r["date"]["value"] == "2019-12-31"));
    }
}

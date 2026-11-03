//! CourtListener bulk-data adapter (PostgreSQL CSV exports `opinions-*.csv`
//! and `opinion-clusters-*.csv`, snapshot 2022-11-30).
//!
//! * **Rights.** CourtListener describes its bulk opinions as free of known
//!   copyright restrictions (Public Domain Mark); judicial opinions are
//!   public domain under the government edicts doctrine.
//! * **Text.** Only opinions with a structured HTML source (`html_lawbox`,
//!   `html_columbia`, `html_anon_2020`, then court-website `html`) are
//!   rendered. Opinions with only `xml_harvard` (the Caselaw Access Project
//!   text, taken from its own adapter), only PDF-layout `plain_text`, or an
//!   OCR flag are rejected. Cluster fields (syllabus, headnotes, summary)
//!   are never read; headnote, syllabus, footnote and caption elements,
//!   footnote paragraphs and star-paging markers break runs.
//! * **Groups.** Unit and source family: one opinion
//!   (`courtlistener_opinion:<id>`). Split group, bucket and `writer_key`:
//!   the linked author person (`courtlistener_person:<author_id>`).
//!   Opinions without a linked author or marked per curiam are rejected.
//!   Clusters with an authored opinion outside the bucket range are listed
//!   in `blocked-case-keys.txt`; `select-candidates` drops their other
//!   opinions so no case spans rendered and reserved buckets.
//! * **Dates.** The cluster `date_filed` must be before 2022-11-30.
use crate::{
    adapter::Run,
    html, legal,
    record::{self, Doc},
    text::{self, Para, Screen},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::BufRead,
    path::Path,
};

pub const COLLECTION: &str = "courtlistener_opinions_historical";
pub const COHORT: &str = "legal_gov_courtlistener_v1";
pub const HTML_FIELDS: &[&str] = &["html_lawbox", "html_columbia", "html_anon_2020", "html"];
pub const DROP_CLASSES: &[&str] = &[
    "headnote", "headnotes", "syllabus", "synopsis", "summary", "footnote", "footnotes", "prelims", "head-matter", "parties",
    "attorneys", "counsel", "docket", "caption", "casename", "citation", "citations", "star-pagination", "page-number", "pagenum",
    "page-label", "num", "jfootnote", "footnote_body", "footnotes_body",
];

/// Streaming reader of PostgreSQL CSV: comma separated, `"` quoted with
/// `""` escapes, records end at an unquoted newline. Fields are UTF-8.
pub struct CsvReader<R> {
    r: R,
    pub records: u64,
}

impl<R: BufRead> CsvReader<R> {
    pub fn new(r: R) -> Self {
        CsvReader { r, records: 0 }
    }
    /// Next record, `None` at end of input.
    pub fn next_record(&mut self) -> Result<Option<Vec<String>>> {
        let mut fields = Vec::new();
        let mut cur: Vec<u8> = Vec::new();
        let mut quoted = false;
        let mut started = false;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = self.r.read_until(b'\n', &mut line)?;
            if n == 0 {
                if !started {
                    return Ok(None);
                }
                if quoted {
                    bail!("unterminated quoted field at end of input");
                }
                fields.push(String::from_utf8(std::mem::take(&mut cur)).context("utf8 field")?);
                self.records += 1;
                return Ok(Some(fields));
            }
            started = true;
            let mut i = 0;
            while i < line.len() {
                let c = line[i];
                if quoted {
                    if c == b'"' {
                        if line.get(i + 1) == Some(&b'"') {
                            cur.push(b'"');
                            i += 2;
                            continue;
                        }
                        quoted = false;
                    } else {
                        cur.push(c);
                    }
                } else if c == b'"' {
                    quoted = true;
                } else if c == b',' {
                    fields.push(String::from_utf8(std::mem::take(&mut cur)).context("utf8 field")?);
                } else if c == b'\n' {
                    fields.push(String::from_utf8(std::mem::take(&mut cur)).context("utf8 field")?);
                    self.records += 1;
                    return Ok(Some(fields));
                } else if c != b'\r' || line.get(i + 1) != Some(&b'\n') {
                    cur.push(c);
                }
                i += 1;
            }
        }
    }
}

pub struct Cluster {
    pub date: Box<str>,
    pub approx: bool,
    pub status: Box<str>,
    pub source: Box<str>,
    pub slug: Box<str>,
}

fn col(header: &[String], name: &str) -> Result<usize> {
    header.iter().position(|h| h == name).with_context(|| format!("column {name}"))
}

/// Cluster id -> date, precedential status, source and slug.
pub fn load_clusters<R: BufRead>(r: R) -> Result<(HashMap<u64, Cluster>, u64)> {
    let mut csv = CsvReader::new(r);
    let header = csv.next_record()?.context("cluster header")?;
    let (id, df, ap, ps, so, sl) = (col(&header, "id")?, col(&header, "date_filed")?, col(&header, "date_filed_is_approximate")?,
                                    col(&header, "precedential_status")?, col(&header, "source")?, col(&header, "slug")?);
    let mut out = HashMap::new();
    let mut bad = 0u64;
    while let Some(rec) = csv.next_record()? {
        let Some(cid) = rec.get(id).and_then(|s| s.parse::<u64>().ok()) else {
            bad += 1;
            continue;
        };
        let g = |k: usize| rec.get(k).map(|s| s.as_str()).unwrap_or("").into();
        out.insert(cid, Cluster { date: g(df), approx: rec.get(ap).is_some_and(|s| s == "t"), status: g(ps), source: g(so), slug: g(sl) });
    }
    Ok((out, bad))
}

fn star_paging(p: &str) -> bool {
    let b = p.as_bytes();
    b.windows(2).any(|w| w[0] == b'*' && w[1].is_ascii_digit())
}

pub fn paragraphs(html_text: &str) -> Vec<Para> {
    html::render_with(html_text, Screen::MARKUP, DROP_CLASSES)
        .into_iter()
        .map(|p| {
            if !p.prose {
                p
            } else if legal::footnote_like(&p.text) {
                Para::non_prose(p.text, "footnote_or_numbered_point")
            } else if star_paging(&p.text) {
                Para::non_prose(p.text, "star_paging_marker")
            } else {
                p
            }
        })
        .collect()
}

pub struct Stats {
    pub by_field: BTreeMap<String, u64>,
    pub blocked_cases: HashSet<u64>,
    pub admitted_soft_per_100: Vec<f64>,
    pub missing_cluster: u64,
}

pub fn adapt<R: BufRead>(opinions: R, clusters: &HashMap<u64, Cluster>, run: &mut Run, out: &Path) -> Result<Value> {
    let method = format!("courtlistener_structured_html_opinion;slop_ninja_post_html_paragraphs_v1_with_class_drops;footnotes_and_star_paging_break;{}", text::PARAGRAPH_RUN_METHOD);
    let mut csv = CsvReader::new(opinions);
    let header = csv.next_record()?.context("opinions header")?;
    let c = |n: &str| col(&header, n);
    let (id, author, per_curiam, typ, cluster, ocr, harvard, plain) =
        (c("id")?, c("author_id")?, c("per_curiam")?, c("type")?, c("cluster_id")?, c("extracted_by_ocr")?, c("xml_harvard")?, c("plain_text")?);
    let html_cols: Vec<(usize, &str)> = HTML_FIELDS.iter().map(|f| Ok((c(f)?, *f))).collect::<Result<_>>()?;
    let mut st = Stats { by_field: BTreeMap::new(), blocked_cases: HashSet::new(), admitted_soft_per_100: Vec::new(), missing_cluster: 0 };
    while let Some(rec) = csv.next_record()? {
        if run.full() {
            break;
        }
        run.units_seen += 1;
        let g = |k: usize| rec.get(k).map(|s| s.as_str()).unwrap_or("");
        let Ok(oid) = g(id).parse::<u64>() else {
            run.count("opinion_without_id");
            continue;
        };
        let unit = format!("courtlistener_opinion:{oid}");
        let cid = g(cluster).parse::<u64>().ok();
        let Some(aid) = g(author).parse::<u64>().ok() else {
            run.count("no_linked_author_person");
            continue;
        };
        let gk = format!("courtlistener_person:{aid}");
        if !run.in_range(&gk) {
            if let Some(cid) = cid {
                st.blocked_cases.insert(cid);
            }
        }
        let field = html_cols.iter().find(|(k, _)| !g(*k).trim().is_empty());
        let cl = cid.and_then(|x| clusters.get(&x));
        let meta = || {
            json!({"opinion_id": oid, "cluster_id": cid, "author_person": aid, "type": g(typ), "per_curiam": g(per_curiam) == "t",
                   "text_field": field.map(|f| f.1), "date_filed": cl.map(|x| &*x.date), "precedential_status": cl.map(|x| &*x.status),
                   "eligible_metadata": field.is_some() && g(ocr) != "t" && g(per_curiam) != "t"
                       && cl.is_some_and(|x| record::before_cutoff(&x.date))})
        };
        if !run.sampled_meta_grouped(&gk, &unit, meta)? {
            continue;
        }
        let Some(cl) = cl else {
            st.missing_cluster += 1;
            run.reject(&unit, "cluster_missing")?;
            continue;
        };
        let reason = if g(per_curiam) == "t" {
            Some("per_curiam")
        } else if g(ocr) == "t" {
            Some("extracted_by_ocr")
        } else if !record::before_cutoff(&cl.date) {
            Some("date_filed_not_before_2022_11_30")
        } else if field.is_none() && !g(harvard).trim().is_empty() {
            Some("harvard_xml_only_taken_from_cap_adapter")
        } else if field.is_none() && !g(plain).trim().is_empty() {
            Some("pdf_layout_plain_text_only_not_reconstructed")
        } else if field.is_none() {
            Some("no_structured_html_text")
        } else {
            None
        };
        if let Some(r) = reason {
            run.reject(&unit, r)?;
            continue;
        }
        let (k, fname) = *field.expect("checked");
        let raw = g(k);
        let paras = paragraphs(raw);
        let Some(ex) = text::paragraph_run(&paras) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            continue;
        };
        if let Some(r) = text::english_screen(&ex.text) {
            run.reject(&unit, r)?;
            continue;
        }
        let ocr_st = legal::ocr_stats(&ex.text);
        if let Some(r) = ocr_st.reason() {
            run.reject(&unit, r)?;
            continue;
        }
        let cid = cid.expect("cluster present");
        let url = format!("https://www.courtlistener.com/opinion/{cid}/{}/", cl.slug);
        let doc = Doc {
            id: format!("pool-courtlistener-{oid}"),
            collection: COLLECTION,
            domain: "legal",
            source_group: unit.clone(),
            writer_key: Some(gk.clone()),
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind": "courtlistener_opinion", "span_basis": "rendered_paragraph_index_half_open", "text_field": fname,
                         "opinion_type": g(typ), "field_sha256": text::sha_hex(raw.as_bytes()), "ocr": ocr_st.to_value()}),
            provenance: json!({"source_url": url, "opinion_id": oid, "cluster_id": cid, "case_key": format!("cl_cluster:{cid}"),
                               "author_person": aid, "date_filed": &*cl.date, "date_filed_is_approximate": cl.approx,
                               "precedential_status": &*cl.status, "cluster_source": &*cl.source, "snapshot": "courtlistener bulk 2022-11-30",
                               "group_key": gk, "group_basis": "linked_author_person", "cohort_id": COHORT}),
            date: Some(cl.date.to_string()),
            date_basis: "courtlistener_cluster_date_filed",
            cutoff_policy: "dated_before_cutoff",
            rights: legal::public_domain_rights(
                cap_license(),
                format!("CourtListener opinion {oid} (cluster {cid}), filed {}; Free Law Project; {url}", cl.date),
                "https://wiki.free.law/c/courtlistener/help/api/bulk-data/bulk-legal-data",
                "Judicial opinion (government edicts doctrine); CourtListener bulk data are described as free of known copyright restrictions. Cluster syllabus, headnote and summary fields are not read.",
                COHORT,
            ),
            aliases: vec![format!("cl_cluster:{cid}")],
        };
        *st.by_field.entry(fname.to_owned()).or_default() += 1;
        if run.accept(&unit, &doc)? {
            st.admitted_soft_per_100.push(ocr_st.soft_per_100());
        }
    }
    let mut blocked: Vec<String> = st.blocked_cases.iter().map(|c| format!("cl_cluster:{c}")).collect();
    blocked.sort();
    std::fs::write(out.join("blocked-case-keys.txt"), blocked.join("\n") + if blocked.is_empty() { "" } else { "\n" })?;
    let mut a = st.admitted_soft_per_100;
    a.sort_by(f64::total_cmp);
    let q = |p: f64| a.get(((a.len() as f64 - 1.0) * p).round() as usize).copied();
    Ok(json!({"csv_records": csv.records, "rendered_by_text_field": st.by_field, "missing_cluster": st.missing_cluster,
              "blocked_case_keys": {"file": "blocked-case-keys.txt", "count": blocked.len(),
                                    "rule": "clusters with an opinion whose linked author lies outside the bucket range; select-candidates drops their other opinions"},
              "ocr_screen": {"method": legal::OCR_SCREEN_METHOD, "admitted_soft_per_100_words": {"n": a.len(), "p50": q(0.5), "p90": q(0.9), "p99": q(0.99)}}}))
}

fn cap_license() -> &'static str {
    crate::cap::LICENSE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_reader_handles_quotes_and_newlines() {
        let s = "id,text,n\n1,\"a, \"\"b\"\"\nc\",2\n2,,3\n";
        let mut r = CsvReader::new(s.as_bytes());
        assert_eq!(r.next_record().unwrap().unwrap(), vec!["id", "text", "n"]);
        assert_eq!(r.next_record().unwrap().unwrap(), vec!["1", "a, \"b\"\nc", "2"]);
        assert_eq!(r.next_record().unwrap().unwrap(), vec!["2", "", "3"]);
        assert!(r.next_record().unwrap().is_none());
    }

    fn csv_field(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }

    #[test]
    fn structured_html_opinions_only_grouped_by_author() {
        let p = |n: usize| format!("<p>{} that the agency acted within its statutory authority.</p>", vec!["We conclude"; n].join(" and "));
        let body = format!("<div class=\"headnotes\"><p>HEADNOTE written by an editor that must never appear here at all.</p></div><p>Opinion of the court.</p>{}{}<div class=\"footnotes\"><p>1 Footnote text.</p></div>", p(30), p(30));
        let clusters = "id,date_filed,date_filed_is_approximate,precedential_status,source,slug\n10,1999-05-01,f,Published,L,roe-v-doe\n11,2023-01-02,f,Published,C,late\n";
        let (cl, _) = load_clusters(clusters.as_bytes()).unwrap();
        let header = "id,author_id,per_curiam,type,cluster_id,extracted_by_ocr,xml_harvard,plain_text,html_lawbox,html_columbia,html_anon_2020,html";
        let row = |id: u64, a: &str, pc: &str, c: u64, ocr: &str, harv: &str, plain: &str, lawbox: &str| {
            format!("{id},{a},{pc},020lead,{c},{ocr},{},{},{},,,", csv_field(harv), csv_field(plain), csv_field(lawbox))
        };
        let rows = [
            row(1, "5", "f", 10, "f", "", "", &body),
            row(2, "", "f", 10, "f", "", "", &body),
            row(3, "5", "t", 10, "f", "", "", &body),
            row(4, "6", "f", 10, "f", "<opinion/>", "", ""),
            row(5, "6", "f", 10, "f", "", "plain text", ""),
            row(6, "6", "f", 11, "f", "", "", &body),
            row(7, "6", "f", 10, "t", "", "", &body),
        ];
        let input = format!("{header}\n{}\n", rows.join("\n"));
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "courtlistener", COLLECTION, json!({}), None).unwrap();
        adapt(input.as_bytes(), &cl, &mut run, &out).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        let r = &rep["rejection_reasons"];
        assert_eq!(r["per_curiam"], 1);
        assert_eq!(r["harvard_xml_only_taken_from_cap_adapter"], 1);
        assert_eq!(r["pdf_layout_plain_text_only_not_reconstructed"], 1);
        assert_eq!(r["date_filed_not_before_2022_11_30"], 1);
        assert_eq!(r["extracted_by_ocr"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["no_linked_author_person"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["writer_key"], "courtlistener_person:5");
        let t = rows[0]["text"].as_str().unwrap();
        assert!(!t.contains("HEADNOTE") && !t.contains("Footnote"));
        assert_eq!(rows[0]["provenance"]["case_key"], "cl_cluster:10");
    }
}

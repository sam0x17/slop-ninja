//! Daily Compilation of Presidential Documents adapter (GovInfo
//! `DCPD-*.htm`, HTML converted from the published PDF).
//!
//! * **Rights.** Presidential remarks and interviews are US government
//!   works (17 U.S.C. 105).
//! * **Spoken categories only.** Remarks, addresses, news conferences,
//!   interviews and exchanges with reporters, by title. Proclamations,
//!   statements, letters, messages, orders and memoranda (staff-drafted
//!   written documents) are rejected.
//! * **President's own words.** Where the document labels speakers, only
//!   turns labelled `The President` or `President <surname of the
//!   administration>` are read; other speakers (foreign leaders, often
//!   through interpreters, reporters, moderators) break runs. Labels are
//!   removed; audience-reaction markers are removed and counted; other
//!   bracketed annotations break; the editorial `NOTE:` and the trailing
//!   index metadata are never read.
//! * **Groups.** Unit and source family: one document
//!   (`govinfo_dcpd:<package>`). Split group, bucket and `writer_key`: the
//!   president (`us_president:<canonical key>` from the `Administration of`
//!   line; known surnames map to one fixed key per president).
use crate::{
    adapter::Run,
    hearings::MONTHS,
    legal,
    record::{self, Doc},
    text::{self, Para},
    wikitext::decode_entities,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;

pub const COLLECTION: &str = "govinfo_presidential_documents_historical";
pub const COHORT: &str = "legal_gov_govinfo_dcpd_v1";
pub const LICENSE: &str = "public-domain-us-federal-government";

pub const SPOKEN_TITLES: &[&str] = &[
    "Remarks", "Address", "The President's News Conference", "The President's Weekly Address", "Interview", "Exchange With Reporters",
    "Question-and-Answer", "Inaugural Address", "Commencement Address", "Joint Press Conference", "The President's Radio Address",
    "News Conference", "Videotaped Remarks", "Teleconference Remarks", "Telephone Remarks", "Roundtable",
];

/// One `<P>` paragraph: plain text, and its leading italic label if any.
pub struct HPara {
    pub text: String,
    pub label: Option<String>,
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    text::squash(&decode_entities(&out))
}

/// Split the converted HTML at `<P` start tags.
pub fn html_paragraphs(html: &str) -> Vec<HPara> {
    let body = html.find("<BODY").or_else(|| html.find("<body")).map(|i| &html[i..]).unwrap_or(html);
    let mut out = Vec::new();
    let upper = body.to_ascii_uppercase();
    let mut starts: Vec<usize> = upper.match_indices("<P").filter(|(i, _)| upper[i + 2..].starts_with(|c: char| c.is_whitespace() || c == '>')).map(|(i, _)| i).collect();
    starts.push(body.len());
    for w in starts.windows(2) {
        let chunk = &body[w[0]..w[1]];
        let Some(gt) = chunk.find('>') else { continue };
        let inner = &chunk[gt + 1..];
        let label = {
            // A leading <I>Label.</I> before any other text.
            let lead = inner.trim_start();
            let li = lead.to_ascii_uppercase();
            let mut l = None;
            let pre_text = |s: &str| strip_tags(s).is_empty();
            if let Some(i) = li.find("<I>") {
                if pre_text(&lead[..i]) {
                    if let Some(j) = li[i..].find("</I>") {
                        let cand = strip_tags(&lead[i + 3..i + j]);
                        if cand.ends_with('.') && cand.split_whitespace().count() <= 6 {
                            l = Some((cand.trim_end_matches('.').to_owned(), i + j + 4));
                        }
                    }
                }
            }
            l
        };
        match label {
            Some((lab, end)) => {
                let lead = inner.trim_start();
                out.push(HPara { text: strip_tags(&lead[end..]), label: Some(lab) });
            }
            None => out.push(HPara { text: strip_tags(inner), label: None }),
        }
    }
    out
}

pub fn parse_date(s: &str) -> Option<String> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    if toks.len() < 3 {
        return None;
    }
    let m = MONTHS.iter().position(|x| *x == toks[0].to_lowercase())?;
    let d: u32 = toks[1].trim_end_matches(',').parse().ok()?;
    let y: u32 = toks[2].trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    ((1..=31).contains(&d) && (1900..=2100).contains(&y)).then(|| format!("{y:04}-{:02}-{d:02}", m + 1))
}

pub struct Parsed {
    pub president: String,
    pub title: String,
    pub date: Option<String>,
    pub paras: Vec<Para>,
    pub labelled: bool,
    pub markers_removed: usize,
    pub other_speaker_paras: usize,
}

pub fn parse(html: &str) -> Option<Parsed> {
    let ps = html_paragraphs(html);
    let ai = ps.iter().position(|p| p.text.starts_with("Administration of "))?;
    let president = ps[ai].text["Administration of ".len()..].rsplit_once(',').map(|(n, _)| n.trim().to_owned())?;
    let title = ps.get(ai + 1)?.text.clone();
    let date = ps.get(ai + 2).and_then(|p| parse_date(&p.text));
    let surname = president.split_whitespace().map(|w| w.trim_end_matches(',')).filter(|w| !w.ends_with('.')).last().unwrap_or("").to_owned();
    let body: Vec<&HPara> = ps[(ai + 3).min(ps.len())..]
        .iter()
        .take_while(|p| !p.text.starts_with("NOTE:") && !p.text.starts_with("Categories:") && !p.text.starts_with("DCPD Number:"))
        .collect();
    let labelled = body.iter().any(|p| p.label.is_some());
    let is_president = |l: &str| l == "The President" || l == format!("President {surname}");
    let mut paras = Vec::new();
    let mut markers = 0usize;
    let mut other = 0usize;
    let mut speaking = !labelled;
    for p in body {
        if let Some(l) = &p.label {
            speaking = is_president(l);
        }
        if !speaking {
            other += 1;
            paras.push(Para::non_prose(String::new(), "other_speaker"));
            continue;
        }
        paras.push(legal::transcript_para(&p.text, &mut markers));
    }
    Some(Parsed { president, title, date, paras, labelled, markers_removed: markers, other_speaker_paras: other })
}

/// Canonical president key: the administration line varies (`Barack Obama`,
/// `Barack H. Obama`, a line merged with the title), so a known surname
/// selects one fixed key; anything else falls back to the name key.
pub fn president_key(name: &str) -> String {
    let toks = text::folded_tokens(name);
    let has = |w: &str| toks.iter().any(|t| t == w);
    let k = if has("obama") {
        "barack_obama".to_owned()
    } else if has("trump") {
        "donald_j_trump".to_owned()
    } else if has("biden") {
        "joseph_r_biden_jr".to_owned()
    } else if has("bush") {
        "george_w_bush".to_owned()
    } else {
        text::underscore_key(name)
    };
    format!("us_president:{k}")
}

pub fn spoken(title: &str) -> bool {
    SPOKEN_TITLES.iter().any(|t| title.starts_with(t))
}

pub fn adapt(files: &[PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("dcpd_html_p_paragraphs;spoken_titles_only;president_turns_only;labels_removed;note_and_index_excluded;{};{}", legal::MARKUP_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut by_president: std::collections::BTreeMap<String, u64> = Default::default();
    let mut markers = 0u64;
    for f in files {
        if run.full() {
            break;
        }
        run.units_seen += 1;
        let pkg = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
        let unit = format!("govinfo_dcpd:{pkg}");
        let bytes = std::fs::read(f)?;
        let sha = text::sha_hex(&bytes);
        let html = String::from_utf8_lossy(&bytes);
        let Some(p) = parse(&html) else {
            run.reject(&unit, "no_administration_title_date_header")?;
            continue;
        };
        let gk = president_key(&p.president);
        let meta = || json!({"package": pkg, "file_sha256": sha, "president_key": gk, "title_sha256": text::sha_hex(p.title.as_bytes()),
                             "date": p.date, "spoken_title": spoken(&p.title), "eligible_metadata": spoken(&p.title)});
        if !run.sampled_meta_grouped(&gk, &unit, meta)? {
            continue;
        }
        let Some(date) = p.date.clone() else {
            run.reject(&unit, "no_document_date")?;
            continue;
        };
        if !record::before_cutoff(&date) {
            run.reject(&unit, "document_date_not_before_2022_11_30")?;
            continue;
        }
        if !spoken(&p.title) {
            run.reject(&unit, "written_document_category")?;
            continue;
        }
        let Some(ex) = text::paragraph_run(&p.paras) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            continue;
        };
        if let Some(r) = text::english_screen(&ex.text) {
            run.reject(&unit, r)?;
            continue;
        }
        let ocr = legal::ocr_stats(&ex.text);
        if let Some(r) = ocr.reason() {
            run.reject(&unit, r)?;
            continue;
        }
        let url = format!("https://www.govinfo.gov/content/pkg/{pkg}/html/{pkg}.htm");
        let doc = Doc {
            id: format!("pool-dcpd-{pkg}"),
            collection: COLLECTION,
            domain: "government_speech",
            source_group: unit.clone(),
            writer_key: Some(gk.clone()),
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind": "presidential_document_president_turns", "span_basis": "body_paragraph_index_half_open",
                         "speaker_labelled": p.labelled, "other_speaker_paragraphs": p.other_speaker_paras,
                         "reaction_markers_removed": p.markers_removed, "file_sha256": sha, "ocr": ocr.to_value(),
                         "unit_note": "the president's own paragraphs of a spoken-category document; speechwriter drafting is not separated"}),
            provenance: json!({"source_url": url, "package": pkg, "title": p.title, "document_date": date, "president": p.president,
                               "group_key": gk, "group_basis": "president", "cohort_id": COHORT}),
            date: Some(date.clone()),
            date_basis: "dcpd_document_date_line",
            cutoff_policy: "dated_before_cutoff",
            rights: legal::public_domain_rights(
                LICENSE,
                format!("{}, {date}; Daily Compilation of Presidential Documents {pkg}; {url}", p.title),
                "https://www.govinfo.gov/help/cpd",
                "US government work (17 U.S.C. 105). Spoken categories only; other speakers, the editorial note and index metadata are excluded.",
                COHORT,
            ),
            aliases: vec![],
        };
        if run.accept(&unit, &doc)? {
            *by_president.entry(gk).or_default() += 1;
            markers += p.markers_removed as u64;
        }
    }
    Ok(json!({"candidates_by_president": by_president, "reaction_markers_removed_in_candidates": markers}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(title: &str, body: &str) -> String {
        format!("<HTML><BODY><DIV><P\n\n><I>Administration of Barack Obama, 2015 </I></P\n><P\n\n><B>{title} </P\n><P\n\n></B><I>January 25, 2015 </I></P\n>{body}<P\n\n>NOTE: The President spoke at 1 p.m. in the East Room of the White House.</P\n><P>Categories: Remarks.</P></BODY></HTML>")
    }

    #[test]
    fn president_turns_only_from_spoken_documents() {
        let s = vec!["We are going to keep working on this together for the American people."; 6].join(" ");
        let body = format!("<P\n\n><FONT size=\"+1\"><I>Prime Minister Doe.</I> Translated words of another leader that are long enough to count here.</P\n><P\n\n><I>The President.</I> {s} [Laughter]</P\n><P\n\n>{s}</P\n><P\n\n><I>Q.</I> A reporter asks a long question about the policy today and tomorrow, sir?</P\n>");
        let p = parse(&page("Remarks With Prime Minister Doe and an Exchange With Reporters", &body)).unwrap();
        assert_eq!(p.president, "Barack Obama");
        assert_eq!(p.date.as_deref(), Some("2015-01-25"));
        assert!(p.labelled);
        assert_eq!(p.markers_removed, 1);
        assert_eq!(p.other_speaker_paras, 2);
        let ex = text::paragraph_run(&p.paras).unwrap();
        assert!(!ex.text.contains("Translated") && !ex.text.contains("reporter") && !ex.text.contains("NOTE"));
        assert!(!ex.text.contains("Laughter"));
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("DCPD-201500001.htm");
        let b = d.path().join("DCPD-201500002.htm");
        std::fs::write(&a, page("Remarks at a Synthetic Event", &format!("<P>{s}</P><P>{s}</P>"))).unwrap();
        std::fs::write(&b, page("Proclamation 9999—Synthetic Day", &format!("<P>{s}</P><P>{s}</P>"))).unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "govinfo-cpd", COLLECTION, json!({}), None).unwrap();
        adapt(&[a, b], &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["rejection_reasons"]["written_document_category"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["writer_key"], "us_president:barack_obama");
        assert_eq!(president_key("Barack H. Obama"), "us_president:barack_obama");
        assert_eq!(president_key("Joseph R. Biden, Jr."), "us_president:joseph_r_biden_jr");
    }
}

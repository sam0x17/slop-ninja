//! Caselaw Access Project adapter over static.case.law per-volume zips
//! (`<reporter>/<volume>.zip` holding `json/<case>.json`).
//!
//! * **Rights.** Judicial opinions are public domain under the government
//!   edicts doctrine; the CAP data were released without restriction in
//!   2024 (a formal CC0 statement on case.law is not confirmed, recorded).
//! * **Text.** Only `casebody.opinions[].text` is read. `head_matter`
//!   (publisher syllabi, headnotes, attorneys, parties) is never read.
//!   Footnote bodies, numbered points and star-paging markers break
//!   paragraph runs. Prose is never repaired.
//! * **Groups.** Unit and source family: one opinion
//!   (`cap_opinion:<case id>:<index>`). Split group, bucket and
//!   `writer_key`: the printed author reduced to one surname within the
//!   jurisdiction (`cap_judge:j<jurisdiction id>:<surname>`). Opinions
//!   without a single identifiable author (per curiam, several authors,
//!   unparsed) are rejected. A case with any authored opinion outside the
//!   run's bucket range is skipped whole, so no case spans rendered and
//!   reserved buckets.
//! * **Screens.** Puerto Rico (translated opinions) is excluded; English
//!   screen; OCR artefact screen (`legal::ocr_stats`) with the per-case
//!   `analysis.ocr_confidence` recorded.
use crate::{
    adapter::Run,
    legal,
    record::{self, Doc},
    text::{self, Para, Screen},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::Path};

pub const COLLECTION: &str = "caselaw_access_project_historical";
pub const LICENSE: &str = "public-domain-us-judicial-opinion";
pub const COHORT: &str = "legal_gov_caselaw_access_project_v1";
const MAX_CASE_BYTES: u64 = 64 * 1024 * 1024;

pub fn group_key(jurisdiction_id: i64, author: &str) -> Option<String> {
    legal::judge_surname(author).map(|s| format!("cap_judge:j{jurisdiction_id}:{s}"))
}

/// Judicial markers that make a short first line an author line.
const JUDGE_MARKERS: &[&str] = &["j", "jj", "justice", "judge", "cj", "pj", "chancellor", "commissioner", "referee", "presiding", "delivered"];

/// The printed author: the `author` field, or else a first text line of at
/// most eight words carrying a judicial marker (`SMITH, J.`, `Mr. Justice
/// Holmes delivered ...`). Returns the line and its basis.
pub fn author_line(o: &Value) -> (String, &'static str) {
    let a = o["author"].as_str().unwrap_or("").trim();
    if !a.is_empty() {
        return (a.to_owned(), "cap_author_field");
    }
    let first = o["text"].as_str().unwrap_or("").lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let toks = text::folded_tokens(first);
    if !toks.is_empty() && toks.len() <= 12 && text::words(first) <= 8 && toks.iter().any(|t| JUDGE_MARKERS.contains(&t.as_str())) {
        return (first.to_owned(), "opinion_first_line");
    }
    (String::new(), "none")
}

pub fn paragraphs(opinion_text: &str) -> Vec<Para> {
    opinion_text
        .split('\n')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            if legal::footnote_like(p) {
                Para::non_prose(text::squash(p), "footnote_or_numbered_point")
            } else {
                Para::classify(text::squash(p), Screen::XML)
            }
        })
        .collect()
}

#[derive(Default)]
pub struct Stats {
    pub ocr_confidence_bins: BTreeMap<String, u64>,
    pub admitted_soft_per_100: Vec<f64>,
    pub screened_ocr: u64,
    pub volumes: Vec<Value>,
}

fn bin(c: Option<f64>) -> String {
    match c {
        None => "none".into(),
        Some(x) => format!("{:.1}", (x * 10.0).floor() / 10.0),
    }
}

pub fn adapt(zips: &[std::path::PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("cap_casebody_opinions_text_newline_paragraphs;head_matter_excluded;footnotes_break;{}", text::PARAGRAPH_RUN_METHOD);
    let mut st = Stats::default();
    for zp in zips {
        if run.full() {
            break;
        }
        volume(zp, &method, run, &mut st)?;
    }
    let mut a = st.admitted_soft_per_100.clone();
    a.sort_by(f64::total_cmp);
    let q = |p: f64| a.get(((a.len() as f64 - 1.0) * p).round() as usize).copied();
    Ok(json!({"volumes": st.volumes.len(), "volume_files": st.volumes,
              "ocr_confidence_bins_of_rendered_opinions": st.ocr_confidence_bins,
              "ocr_screen": {"method": legal::OCR_SCREEN_METHOD, "rejected": st.screened_ocr,
                              "admitted_soft_per_100_words": {"n": a.len(), "p50": q(0.5), "p90": q(0.9), "p99": q(0.99), "max": a.last()}}}))
}

fn volume(zp: &Path, method: &str, run: &mut Run, st: &mut Stats) -> Result<()> {
    let (sha, bytes) = crate::io::file_sha(zp)?;
    let vol = zp.file_stem().and_then(|s| s.to_str()).unwrap_or("").replace("__", "/");
    let mut z = zip::ZipArchive::new(std::fs::File::open(zp)?).with_context(|| format!("zip {}", zp.display()))?;
    let mut names: Vec<(String, usize)> = (0..z.len())
        .filter_map(|i| z.by_index_raw(i).ok().map(|e| (e.name().to_owned(), i)))
        .filter(|(n, _)| n.starts_with("json/") && n.ends_with(".json"))
        .collect();
    names.sort();
    st.volumes.push(json!({"volume": vol, "file": zp.file_name().and_then(|n| n.to_str()), "sha256": sha, "bytes": bytes, "cases": names.len()}));
    for (name, i) in names {
        if run.full() {
            break;
        }
        let mut b = Vec::new();
        {
            let e = z.by_index(i)?;
            if e.size() > MAX_CASE_BYTES {
                run.reject(&format!("cap_file:{vol}/{name}"), "case_file_too_large")?;
                continue;
            }
            e.take(MAX_CASE_BYTES + 1).read_to_end(&mut b)?;
        }
        let Ok(case) = serde_json::from_slice::<Value>(&b) else {
            run.reject(&format!("cap_file:{vol}/{name}"), "case_json_unparsed")?;
            continue;
        };
        one_case(&case, &vol, &sha, method, run, st)?;
    }
    Ok(())
}

pub fn one_case(case: &Value, vol: &str, zip_sha: &str, method: &str, run: &mut Run, st: &mut Stats) -> Result<()> {
    let Some(cid) = case["id"].as_i64() else {
        run.count("case_without_id");
        return Ok(());
    };
    let jur_id = case["jurisdiction"]["id"].as_i64().unwrap_or(-1);
    let jur = case["jurisdiction"]["name_long"].as_str().unwrap_or("");
    let date = case["decision_date"].as_str().unwrap_or("").to_owned();
    let ocr_conf = case["analysis"]["ocr_confidence"].as_f64();
    let empty = vec![];
    let opinions = case["casebody"]["opinions"].as_array().unwrap_or(&empty);
    let authors: Vec<(String, &str)> = opinions.iter().map(author_line).collect();
    let keys: Vec<Option<String>> = authors.iter().map(|(a, _)| group_key(jur_id, a)).collect();
    // A case with an authored opinion outside the range is skipped whole.
    let blocked = keys.iter().flatten().any(|k| !run.in_range(k));
    for (idx, o) in opinions.iter().enumerate() {
        if run.full() {
            break;
        }
        run.units_seen += 1;
        let unit = format!("cap_opinion:{cid}:{idx}");
        let Some(gk) = keys[idx].clone() else {
            run.reject(&unit, "no_single_identified_author")?;
            continue;
        };
        if blocked && run.in_range(&gk) && !run.is_metadata_only() {
            run.reject(&unit, "case_has_opinion_by_judge_outside_rendered_buckets")?;
            continue;
        }
        let otext = o["text"].as_str().unwrap_or("");
        let meta = || {
            json!({"case_id": cid, "opinion_index": idx, "opinion_type": o["type"], "author_sha256": text::sha_hex(authors[idx].0.as_bytes()), "author_basis": authors[idx].1,
                   "jurisdiction_id": jur_id, "court_id": case["court"]["id"], "decision_date": date, "volume": vol, "zip_sha256": zip_sha,
                   "opinion_text_sha256": text::sha_hex(otext.as_bytes()), "ocr_confidence": ocr_conf, "case_blocked_by_other_bucket": blocked,
                   "eligible_metadata": jur != "Puerto Rico"})
        };
        if !run.sampled_meta_grouped(&gk, &unit, meta)? {
            continue;
        }
        if jur == "Puerto Rico" {
            run.reject(&unit, "puerto_rico_translated_opinions_excluded")?;
            continue;
        }
        let year_ok = date.get(..4).and_then(|y| y.parse::<u32>().ok()).is_some_and(|y| y < 2022);
        if !(year_ok || record::before_cutoff(&date)) {
            run.reject(&unit, "decision_date_missing_or_not_before_2022_11_30")?;
            continue;
        }
        *st.ocr_confidence_bins.entry(bin(ocr_conf)).or_default() += 1;
        let paras = paragraphs(otext);
        let Some(ex) = text::paragraph_run(&paras) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            continue;
        };
        if let Some(r) = text::english_screen(&ex.text) {
            run.reject(&unit, r)?;
            continue;
        }
        let ocr = legal::ocr_stats(&ex.text);
        if let Some(r) = ocr.reason() {
            st.screened_ocr += 1;
            run.reject(&unit, r)?;
            continue;
        }
        let name = case["name_abbreviation"].as_str().unwrap_or("");
        let cite = case["citations"].get(0).and_then(|c| c["cite"].as_str()).unwrap_or("");
        let url = format!("https://static.case.law/{vol}/cases/{}.json", case["file_name"].as_str().unwrap_or(""));
        let full_date = record::valid_date(date.get(..10).unwrap_or(""));
        let doc = Doc {
            id: format!("pool-cap-{cid}-o{idx}"),
            collection: COLLECTION,
            domain: "legal",
            source_group: unit.clone(),
            writer_key: Some(gk.clone()),
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: method,
            unit: json!({"kind": "cap_opinion", "span_basis": "rendered_paragraph_index_half_open", "opinion_type": o["type"],
                         "opinion_text_sha256": text::sha_hex(otext.as_bytes()), "ocr": ocr.to_value(), "cap_ocr_confidence": ocr_conf,
                         "volume_zip_sha256": zip_sha}),
            provenance: json!({"source_url": url, "case_id": cid, "case_key": format!("cap_case:{cid}"), "opinion_index": idx,
                               "case_name_abbreviation": name, "citation": cite, "court": case["court"]["name"], "court_id": case["court"]["id"],
                               "jurisdiction": jur, "jurisdiction_id": jur_id, "decision_date": date, "volume": vol,
                               "cap_provenance": case["provenance"], "group_key": gk, "group_basis": "printed_author_surname_within_jurisdiction", "author_basis": authors[idx].1,
                               "cohort_id": COHORT}),
            date: Some(date.clone()),
            date_basis: "cap_decision_date",
            cutoff_policy: if full_date { "dated_before_cutoff" } else { "dated_before_cutoff_partial_date_year_checked" },
            rights: legal::public_domain_rights(
                LICENSE,
                format!("{name}, {cite} ({date}); Caselaw Access Project, Harvard Law School Library; {url}"),
                "https://lil.law.harvard.edu/blog/2024/03/26/transitions-for-the-caselaw-access-project/",
                "Judicial opinion (government edicts doctrine). CAP data released without restriction in 2024; formal CC0 statement on case.law not confirmed. Publisher head matter is not read.",
                COHORT,
            ),
            aliases: vec![format!("cap_case:{cid}")],
        };
        if run.accept(&unit, &doc)? {
            st.admitted_soft_per_100.push(ocr.soft_per_100());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(n: usize) -> String {
        format!("{} that the record supports the finding below.", vec!["The court holds"; n].join(" and "))
    }

    fn case(id: i64, author: &str, body: &str, extra_author: Option<&str>) -> Value {
        let mut ops = vec![json!({"type": "majority", "author": author, "text": body})];
        if let Some(a) = extra_author {
            ops.push(json!({"type": "dissent", "author": a, "text": body}));
        }
        json!({"id": id, "name_abbreviation": "Roe v. Doe", "decision_date": "1950-03-04", "citations": [{"cite": "1 Syn. 1"}],
               "court": {"id": 1, "name": "Synthetic Supreme Court"}, "jurisdiction": {"id": 7, "name_long": "Synthetica"},
               "analysis": {"ocr_confidence": 0.9}, "file_name": "0001-01",
               "casebody": {"head_matter": "SYLLABUS BY THE PUBLISHER that must never appear in the text.", "opinions": ops}})
    }

    #[test]
    fn opinions_render_without_head_matter_and_group_by_judge() {
        let body = format!("SMITH, J.\n{}\n{}\n1 See Roe v. Doe, 1 Syn. 1.", para(20), para(20));
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "cap", COLLECTION, json!({}), None).unwrap();
        let mut st = Stats::default();
        one_case(&case(1, "SMITH, J.", &body, None), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        one_case(&case(2, "Per Curiam.", &body, None), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        // Empty author field: the first text line names the judge; "Affirmed." does not.
        one_case(&case(4, "", &format!("BROWN, J.\n{}\n{}", para(20), para(20)), None), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        one_case(&case(5, "", &format!("Affirmed.\n{}\n{}", para(20), para(20)), None), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        let noisy = format!("JONES, J.\n{} \u{25a0} c1aim.\n{}", para(20), para(20));
        one_case(&case(3, "JONES, J.", &noisy, None), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 2);
        assert_eq!(rep["rejection_reasons"]["no_single_identified_author"], 2);
        assert_eq!(rep["rejection_reasons"]["ocr_glyph_artifact"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["writer_key"], "cap_judge:j7:smith");
        assert_eq!(rows[0]["source_group"], "cap_opinion:1:0");
        let t = rows[0]["text"].as_str().unwrap();
        assert!(!t.contains("SYLLABUS") && !t.contains("SMITH, J.") && !t.contains("See Roe"));
        assert_eq!(rows[0]["extraction"]["unit"]["ocr"]["hard_glyphs"], 0);
        assert_eq!(rows[1]["writer_key"], "cap_judge:j7:brown");
        assert_eq!(rows[1]["provenance"]["author_basis"], "opinion_first_line");
    }

    #[test]
    fn case_with_a_reserved_judge_is_skipped_whole() {
        let name = |i: u32| format!("Q{}{}{}", (b'a' + (i % 26) as u8) as char, (b'a' + (i / 26 % 26) as u8) as char, (b'a' + (i / 676 % 26) as u8) as char);
        let pick = |lo: u32, hi: u32| (0..10000).map(name).find(|s| {
            let k = crate::adapter::bucket(&format!("cap_judge:j7:{}", s.to_lowercase()));
            lo <= k && k < hi
        }).unwrap();
        let (t, r) = (pick(0, 900), pick(900, 1000));
        let body = format!("{}\n{}", para(20), para(20));
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("out");
        let mut run = Run::start_sampled(&out, "cap", COLLECTION, json!({}), None, Some((0, 900))).unwrap();
        let mut st = Stats::default();
        one_case(&case(1, &format!("{t}, J."), &body, Some(&format!("{r}, J."))), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        one_case(&case(2, &format!("{t}, J."), &body, None), "syn/1", "00", "m", &mut run, &mut st).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        assert_eq!(rep["rejection_reasons"]["case_has_opinion_by_judge_outside_rendered_buckets"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["contributor_bucket_outside_run_range"], 1);
    }
}

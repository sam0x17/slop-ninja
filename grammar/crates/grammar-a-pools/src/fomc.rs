//! FOMC meeting and conference-call transcripts (federalreserve.gov PDFs,
//! released with a five-year lag; lightly edited verbatim records).
//!
//! * **Rights.** Works of the Board of Governors and the FOMC, US
//!   government works (17 U.S.C. 105).
//! * **Text.** `pdf_text` extracts the PDF text layer with the pure-Rust
//!   `pdf-extract` crate into one `.txt` per PDF (status recorded per file;
//!   no OCR is performed here). Older transcripts are scanned typescripts
//!   whose text layer is OCR, so the OCR artefact screen applies.
//! * **Turns.** A turn starts at an all-capitals speaker label at line start
//!   (`CHAIRMAN GREENSPAN.`, `MR. KOHN.`, `VICE CHAIRMAN DUDLEY.`). Page
//!   furniture (page numbers, release stamps, lines repeated on many pages)
//!   is dropped and counted. Blank lines are paragraph breaks; a turn
//!   without breaks is one paragraph. The label is removed; bracketed
//!   annotations break runs.
//! * **Groups.** Unit and source family: one turn
//!   (`fomc_turn:<meeting>:<index>`). Split group, bucket and `writer_key`:
//!   the speaker surname (`fomc_speaker:<surname>`); different people with
//!   one surname merge (conservative).
use crate::{
    adapter::Run,
    legal,
    record::Doc,
    text::{self, Para},
};
use anyhow::Result;
use serde_json::{Value, json};
use std::{collections::HashMap, path::{Path, PathBuf}};

pub const COLLECTION: &str = "fomc_transcripts_historical";
pub const COHORT: &str = "legal_gov_fomc_transcripts_v1";
pub const LICENSE: &str = "public-domain-us-federal-government";

/// Extract the text layer of every `*.pdf` in `input` into `out/<stem>.txt`
/// and write `out/pdf-text-report.json`.
pub fn pdf_text(input: &Path, out: &Path) -> Result<Value> {
    crate::io::fresh_dir(out)?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(input)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "pdf")).collect();
    files.sort();
    let mut rows = Vec::new();
    std::panic::set_hook(Box::new(|_| {}));
    for f in &files {
        let bytes = std::fs::read(f)?;
        let sha = text::sha_hex(&bytes);
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
        let r = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes));
        let (status, chars) = match r {
            Ok(Ok(t)) => {
                std::fs::write(out.join(format!("{stem}.txt")), &t)?;
                ("ok", t.chars().count())
            }
            Ok(Err(_)) => ("extract_error", 0),
            Err(_) => ("extract_panic", 0),
        };
        rows.push(json!({"pdf": f.file_name().and_then(|n| n.to_str()), "pdf_sha256": sha, "status": status, "chars": chars}));
    }
    let _ = std::panic::take_hook();
    let rep = json!({"schema": "slop_ninja_pdf_text_report_v1", "extractor": "pdf-extract 0.10.0 (text layer only, no OCR)", "files": rows});
    crate::io::write_json(&out.join("pdf-text-report.json"), &rep)?;
    Ok(rep)
}

const LABEL_HEADS: &[&str] = &["CHAIRMAN", "CHAIRWOMAN", "VICE CHAIRMAN", "VICE CHAIR", "MR.", "MS.", "MRS.", "DR.", "PRESIDENT", "GOVERNOR", "CHAIR"];

/// `MR. KOHN. Thank you.` -> (`MR. KOHN`, `Thank you.`, named). Any other
/// all-capitals label (`SPEAKER(?).`, `SEVERAL.`) starts an unnamed turn,
/// so its words are never credited to the previous speaker.
pub fn label(line: &str) -> Option<(String, String, bool)> {
    named_label(line).map(|(a, b)| (a, b, true)).or_else(|| generic_label(line).map(|(a, b)| (a, b, false)))
}

fn caps_word(core: &str) -> bool {
    let c = core.strip_prefix("Mc").unwrap_or(core);
    c.len() >= 2 && c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '\'' || ch == '-')
}

fn named_label(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let head = LABEL_HEADS.iter().find(|h| t.starts_with(*h) && t[h.len()..].starts_with(' '))?;
    let rest = &t[head.len() + 1..];
    // One or two all-capital name words, then ". " (or end of line).
    let mut end = None;
    let mut pos = 0;
    for (k, w) in rest.split(' ').enumerate() {
        if !caps_word(w.trim_end_matches('.')) {
            return None;
        }
        pos += w.len() + 1;
        if w.ends_with('.') {
            end = Some(pos - 1);
            break;
        }
        if k >= 1 {
            return None;
        }
    }
    let end = end?;
    let name = &rest[..end - 1];
    Some((format!("{head} {name}"), rest[end..].trim().to_owned()))
}

fn generic_label(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let (prefix, rest) = match t.find(". ") {
        Some(i) => (&t[..i], t[i + 2..].trim()),
        None if t.ends_with('.') => (&t[..t.len() - 1], ""),
        None => return None,
    };
    let words: Vec<&str> = prefix.split_whitespace().collect();
    let ok = !words.is_empty() && words.len() <= 4 && !["MR", "MS", "MRS", "DR"].contains(&words[0])
        && words.iter().all(|w| w.chars().all(|c| c.is_ascii_uppercase() || "()?'-.".contains(c)))
        && words.iter().any(|w| w.chars().filter(|c| c.is_ascii_uppercase()).count() >= 2);
    ok.then(|| (prefix.to_owned(), rest.to_owned()))
}

fn page_furniture(l: &str, repeated: &HashMap<String, usize>) -> bool {
    let t = l.trim();
    let low = t.to_lowercase();
    if low.contains("authorized for public release") || low.contains("class i fomc") || low.contains("class ii fomc") || low.contains("restricted fr") {
        return true;
    }
    let digits_only = !t.is_empty() && t.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ' ');
    let page_of = low.starts_with("page ") || (low.contains(" of ") && t.split_whitespace().count() <= 8 && t.split_whitespace().last().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())));
    digits_only || page_of || repeated.get(t).is_some_and(|n| *n >= 5 && t.split_whitespace().count() <= 10 && !text::ends_sentence(t))
}

pub struct Turn {
    pub label: String,
    pub named: bool,
    pub paras: Vec<Para>,
    pub markers_removed: usize,
}

/// Share of the median line width below which a sentence-final line ends
/// its paragraph (the PDF text layer keeps neither indentation nor
/// paragraph spacing; blank lines are double spacing).
pub const SHORT_LINE_SHARE: f64 = 0.85;
pub const PARAGRAPH_RULE: &str = "paragraph_ends_after_sentence_final_line_shorter_than_0.85_median_line_width;blank_lines_ignored";

pub fn turns(txt: &str) -> (Vec<Turn>, usize) {
    let mut repeated: HashMap<String, usize> = HashMap::new();
    for l in txt.lines() {
        *repeated.entry(l.trim().to_owned()).or_default() += 1;
    }
    let mut widths: Vec<usize> = txt.lines().map(|l| l.trim().chars().count()).filter(|n| *n >= 20).collect();
    widths.sort_unstable();
    let median = widths.get(widths.len() / 2).copied().unwrap_or(80) as f64;
    let mut furniture = 0usize;
    let mut out: Vec<Turn> = Vec::new();
    let mut cur_para: Vec<String> = Vec::new();
    let flush = |out: &mut Vec<Turn>, cur: &mut Vec<String>| {
        if let Some(t) = out.last_mut() {
            if !cur.is_empty() {
                let p = cur.join(" ");
                let para = legal::transcript_para(&p, &mut t.markers_removed);
                t.paras.push(para);
            }
        }
        cur.clear();
    };
    for l in txt.lines() {
        if l.trim().is_empty() {
            continue;
        }
        if page_furniture(l, &repeated) {
            furniture += 1;
            continue;
        }
        let line = if let Some((lab, rest, named)) = label(l) {
            flush(&mut out, &mut cur_para);
            out.push(Turn { label: lab, named, paras: Vec::new(), markers_removed: 0 });
            rest
        } else {
            l.trim().to_owned()
        };
        if out.is_empty() || line.is_empty() {
            continue;
        }
        let short_end = text::ends_sentence(&line) && (l.trim().chars().count() as f64) < SHORT_LINE_SHARE * median;
        cur_para.push(line);
        if short_end {
            flush(&mut out, &mut cur_para);
        }
    }
    flush(&mut out, &mut cur_para);
    (out, furniture)
}

/// `FOMC20100127meeting` -> (`2010-01-27`, `meeting`).
pub fn meeting(stem: &str) -> Option<(String, String)> {
    let r = stem.strip_prefix("FOMC")?;
    let d = r.get(..8)?;
    if !d.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]), r[8..].to_owned()))
}

pub fn adapt(text_dir: &Path, run: &mut Run) -> Result<Value> {
    let method = format!("pdf_extract_text_layer;fomc_capitals_speaker_turns;page_furniture_dropped;{PARAGRAPH_RULE};label_removed;{};{}", legal::MARKUP_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut files: Vec<PathBuf> = std::fs::read_dir(text_dir)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "txt")).collect();
    files.sort();
    let mut per_file = Vec::new();
    for f in &files {
        if run.full() {
            break;
        }
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
        let Some((date, kind)) = meeting(&stem) else { continue };
        let raw = std::fs::read_to_string(f)?;
        let sha = text::sha_hex(raw.as_bytes());
        let (ts, furniture) = turns(&raw);
        let mut admitted = 0u64;
        for (i, t) in ts.iter().enumerate() {
            if run.full() {
                break;
            }
            run.units_seen += 1;
            let unit = format!("fomc_turn:{stem}:{i}");
            let Some(sn) = t.named.then(|| legal::speaker_surname(&t.label)).flatten() else {
                run.count("unnamed_speaker_label");
                continue;
            };
            let gk = format!("fomc_speaker:{sn}");
            let meta = || json!({"meeting": stem, "date": date, "kind": kind, "turn_index": i, "speaker_key": gk, "text_sha256": sha, "eligible_metadata": true});
            if !run.sampled_meta_grouped(&gk, &unit, meta)? {
                continue;
            }
            let Some(ex) = text::paragraph_run(&t.paras) else {
                run.count("turn_without_paragraph_run_within_bounds");
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
            let url = format!("https://www.federalreserve.gov/monetarypolicy/files/{stem}.pdf");
            let doc = Doc {
                id: format!("pool-fomc-{stem}-t{i}"),
                collection: COLLECTION,
                domain: "government_speech",
                source_group: unit.clone(),
                writer_key: Some(gk.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "fomc_speaker_turn", "span_basis": "turn_paragraph_index_half_open", "turn_index": i, "label_removed": true,
                             "reaction_markers_removed_in_turn": t.markers_removed, "page_furniture_lines_dropped_in_file": furniture,
                             "text_layer_sha256": sha, "ocr": ocr.to_value(),
                             "unit_note": "one speaker turn; paragraphs reconstructed from short sentence-final lines of the PDF text layer"}),
                provenance: json!({"source_url": url, "meeting": stem, "meeting_date": date, "kind": kind, "speaker_key": gk,
                                   "group_key": gk, "group_basis": "speaker_surname", "cohort_id": COHORT}),
                date: Some(date.clone()),
                date_basis: "fomc_meeting_date_from_file_name",
                cutoff_policy: "dated_before_cutoff",
                rights: legal::public_domain_rights(
                    LICENSE,
                    format!("Transcript of the Federal Open Market Committee {kind} of {date}; Board of Governors of the Federal Reserve System; {url}"),
                    "https://www.federalreserve.gov/monetarypolicy/fomc_historical.htm",
                    "US government work (17 U.S.C. 105); lightly edited verbatim transcript released after five years.",
                    COHORT,
                ),
                aliases: vec![],
            };
            if run.accept(&unit, &doc)? {
                admitted += 1;
            }
        }
        per_file.push(json!({"file": f.file_name().and_then(|n| n.to_str()), "sha256": sha, "turns": ts.len(), "page_furniture_lines": furniture, "candidates": admitted}));
    }
    Ok(json!({"files": per_file}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_turns() {
        assert_eq!(label("CHAIRMAN GREENSPAN. Good morning."), Some(("CHAIRMAN GREENSPAN".into(), "Good morning.".into(), true)));
        assert_eq!(label("VICE CHAIRMAN McDONOUGH. Yes."), Some(("VICE CHAIRMAN McDONOUGH".into(), "Yes.".into(), true)));
        assert_eq!(label("MR. KOHN."), Some(("MR. KOHN".into(), "".into(), true)));
        assert_eq!(label("MR. Kohn said that."), None);
        assert_eq!(label("PRESIDENT OF THE BANK SAID"), None);
        assert_eq!(label("SPEAKER(?). Right."), Some(("SPEAKER(?)".into(), "Right.".into(), false)));
        assert_eq!(label("Inflation is low. We agree."), None);
        // Full-width lines inside paragraphs; a short sentence-final line ends one.
        let full = "inflation expectations remain well anchored in my district this quarter and";
        let para = format!("{}\nwe expect growth.", vec![full; 4].join("\n"));
        let txt = format!("Authorized for Public Release\n\nJanuary 27, 2010 1 of 9\n\nCHAIRMAN BERNANKE. {para}\n\n{para}\n2\nMR. KOHN. Thank you. [Laughter]\nMS. DOE. {para}");
        let (ts, furniture) = turns(&txt);
        assert_eq!(ts.len(), 3);
        assert_eq!(furniture, 3);
        assert_eq!(ts[0].paras.len(), 2);
        assert_eq!(ts[1].markers_removed, 1);
        assert_eq!(meeting("FOMC20100127meeting"), Some(("2010-01-27".into(), "meeting".into())));
    }

    #[test]
    fn transcript_turns_become_documents_grouped_by_speaker() {
        let full = "inflation expectations remain well anchored in my district this quarter and";
        let para = format!("{}\nwe expect growth.", vec![full; 7].join("\n"));
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("FOMC20100127meeting.txt"), format!("CHAIRMAN BERNANKE. {para}\n\n{para}\nSPEAKER(?). {para}\nMR. KOHN. Thanks.")).unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "fomc", COLLECTION, json!({}), None).unwrap();
        adapt(d.path(), &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["writer_key"], "fomc_speaker:bernanke");
        assert!(!rows[0]["text"].as_str().unwrap().contains("BERNANKE"));
    }
}

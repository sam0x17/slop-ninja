//! GovInfo Congressional hearings adapter (`CHRG-*.htm`: GPO plain text in
//! one `<pre>` block).
//!
//! * **Rights.** Hearing transcripts are works of the US government
//!   (17 U.S.C. 105). Submitted material may carry third-party rights; it is
//!   excluded (below).
//! * **Spoken record only.** Text is split into speaker turns at GPO turn
//!   labels (`Mr. Smith.`, `Senator Doe.`, `The Chairman.`). Prepared
//!   statements and other insertions (`[The prepared statement of ...
//!   follows:]`, any bracketed annotation) end the turn's admissible text,
//!   and nothing after an appendix heading is read. Front matter before the
//!   first turn is read only for the hearing date. The turn label is
//!   removed from the excerpt (recorded); audience-reaction markers are
//!   removed and counted.
//! * **Groups.** Source family, split group and bucket: the hearing
//!   (`govinfo_chrg:<package>`). `writer_key`: an opaque per-hearing speaker
//!   key (a SHA-256 prefix of package and label); speaker identity across
//!   hearings is not linked (recorded limit).
//! * **Dates.** The latest hearing date printed in the front matter must be
//!   before 2022-11-30.
use crate::{
    adapter::Run,
    legal,
    record::{self, Doc},
    text::{self, Para},
    wikitext::decode_entities,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;

pub const COLLECTION: &str = "govinfo_congressional_hearings_historical";
pub const COHORT: &str = "legal_gov_govinfo_hearings_v1";
pub const LICENSE: &str = "public-domain-us-federal-government";

const HONORIFICS: &[&str] = &[
    "Mr.", "Ms.", "Mrs.", "Miss", "Dr.", "Senator", "Representative", "Chairman", "Chairwoman", "Chair", "General", "Admiral",
    "Secretary", "Governor", "Ambassador", "Judge", "Justice", "Commissioner", "Director", "Mayor", "Professor", "Colonel",
    "Captain", "Major", "Lieutenant", "Sergeant", "Delegate", "Reverend", "Father", "Sister", "Chief", "Attorney", "Inspector",
];

pub const MONTHS: &[&str] = &["january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december"];

/// `Mr. Bordeaux. Good afternoon` -> (`Mr. Bordeaux`, `Good afternoon`).
pub fn turn_label(p: &str) -> Option<(&str, &str)> {
    let mut cut = p.len().min(80);
    while !p.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &p[..cut];
    let mut search = 0;
    while let Some(i) = head[search..].find(". ") {
        let end = search + i;
        let label = &p[..end];
        let words: Vec<&str> = label.split_whitespace().collect();
        if words.is_empty() {
            return None;
        }
        let ok_shape = !words.is_empty()
            && words.len() <= 6
            && words.iter().all(|w| w.chars().next().is_some_and(|c| c.is_uppercase()) || *w == "of" || *w == "de" || *w == "van")
            && !label.contains(',');
        let ok_start = label.starts_with("The Chair") || label == "The Clerk" || label == "The Reporter" || label == "Voice" || label == "Voices"
            || label == "The Speaker" || label == "The Witness"
            || HONORIFICS.iter().any(|h| words[0] == *h);
        // `Mr.` alone ends at its own full stop; extend to the next one.
        if words.len() == 1 && HONORIFICS.contains(&words[0]) && words[0].ends_with('.') || label == "Mr" || label == "Ms" || label == "Mrs" || label == "Dr" {
            search = end + 2;
            continue;
        }
        if ok_shape && ok_start {
            return Some((label, p[end + 2..].trim_start()));
        }
        return None;
    }
    None
}

/// Dates printed as `JANUARY 28, 2010` or `January 28, 2010`.
pub fn dates_in(s: &str) -> Vec<String> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let mut out = Vec::new();
    for w in toks.windows(3) {
        let m = w[0].trim_matches(|c: char| !c.is_alphabetic()).to_lowercase();
        let Some(mi) = MONTHS.iter().position(|x| *x == m) else { continue };
        let d: String = w[1].chars().filter(|c| c.is_ascii_digit()).collect();
        let y: String = w[2].chars().filter(|c| c.is_ascii_digit()).collect();
        if !w[1].ends_with(',') || d.is_empty() || d.len() > 2 || y.len() != 4 {
            continue;
        }
        let (Ok(d), Ok(y)) = (d.parse::<u32>(), y.parse::<u32>()) else { continue };
        if (1..=31).contains(&d) && (1900..=2100).contains(&y) {
            out.push(format!("{y:04}-{:02}-{d:02}", mi + 1));
        }
    }
    out
}

/// Paragraphs of a GPO `<pre>` body: a paragraph starts at a line indented
/// by exactly four spaces and continues over flush-left lines. Lines indented
/// further, blank lines and rules are headings or layout (breaks).
pub fn gpo_paragraphs(pre: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut cur: Option<String> = None;
    for line in pre.lines() {
        let lead = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() {
            if let Some(c) = cur.take() {
                out.push((c, true));
            }
            continue;
        }
        if lead == 4 && !t.starts_with('_') && !t.starts_with('-') {
            if let Some(c) = cur.take() {
                out.push((c, true));
            }
            cur = Some(t.to_owned());
        } else if lead == 0 && cur.is_some() {
            let c = cur.as_mut().expect("some");
            c.push(' ');
            c.push_str(t);
        } else {
            if let Some(c) = cur.take() {
                out.push((c, true));
            }
            out.push((t.to_owned(), false));
        }
    }
    if let Some(c) = cur {
        out.push((c, true));
    }
    out
}

fn appendix_heading(t: &str) -> bool {
    let u = t.trim();
    u == "A P P E N D I X" || u == "APPENDIX" || u.starts_with("APPENDIX ") || u.starts_with("A P P E N D I X")
}

pub struct Turn {
    pub label: String,
    pub paras: Vec<Para>,
    pub markers_removed: usize,
    pub insertions_cut: usize,
}

/// Speaker turns after the front matter, and the front matter text.
pub fn turns(pre: &str) -> (Vec<Turn>, String) {
    let mut front = String::new();
    let mut out: Vec<Turn> = Vec::new();
    let mut in_insertion = false;
    for (raw, body) in gpo_paragraphs(pre) {
        let t = text::squash(&raw);
        if appendix_heading(&t) {
            break;
        }
        if !body {
            if let Some(cur) = out.last_mut() {
                cur.paras.push(Para::non_prose(t, "heading_or_layout"));
            } else {
                front.push_str(&t);
                front.push('\n');
            }
            continue;
        }
        if let Some((label, rest)) = turn_label(&t) {
            in_insertion = false;
            let mut turn = Turn { label: label.to_owned(), paras: Vec::new(), markers_removed: 0, insertions_cut: 0 };
            let p = legal::transcript_para(rest, &mut turn.markers_removed);
            turn.paras.push(p);
            out.push(turn);
            continue;
        }
        let Some(cur) = out.last_mut() else {
            front.push_str(&t);
            front.push('\n');
            continue;
        };
        if t.starts_with('[') {
            // Prepared statements, submissions and other insertions: excluded
            // until the next turn label.
            in_insertion = true;
            cur.insertions_cut += 1;
            cur.paras.push(Para::non_prose(String::new(), "inserted_or_bracketed_material"));
            continue;
        }
        if in_insertion {
            continue;
        }
        let p = legal::transcript_para(&t, &mut cur.markers_removed);
        cur.paras.push(p);
    }
    (out, front)
}

fn pre_body(html: &str) -> Option<&str> {
    let i = html.find("<pre>")?;
    let j = html.rfind("</pre>")?;
    (j > i).then(|| &html[i + 5..j])
}

pub fn adapt(files: &[PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("gpo_pre_four_space_paragraphs;speaker_turns;label_removed;insertions_and_appendix_excluded;{};{}", legal::MARKUP_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut hearings = 0u64;
    let mut markers = 0u64;
    let mut soft = Vec::new();
    for f in files {
        if run.full() {
            break;
        }
        let pkg = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_owned();
        let family = format!("govinfo_chrg:{pkg}");
        let bytes = std::fs::read(f)?;
        let sha = text::sha_hex(&bytes);
        let meta = || json!({"package": pkg, "file_sha256": sha, "bytes": bytes.len(), "eligible_metadata": true});
        if !run.sampled_meta(&family, meta)? {
            continue;
        }
        hearings += 1;
        let html = String::from_utf8_lossy(&bytes);
        let Some(pre) = pre_body(&html) else {
            run.reject(&family, "no_pre_body")?;
            continue;
        };
        let pre = decode_entities(pre);
        let (ts, front) = turns(&pre);
        let Some(date) = dates_in(&front).into_iter().max() else {
            run.reject(&family, "no_hearing_date_in_front_matter")?;
            continue;
        };
        for (i, turn) in ts.iter().enumerate() {
            if run.full() {
                break;
            }
            run.units_seen += 1;
            let unit = format!("{family}:t{i}");
            if !record::before_cutoff(&date) {
                run.reject(&unit, "hearing_date_not_before_2022_11_30")?;
                continue;
            }
            let Some(ex) = text::paragraph_run(&turn.paras) else {
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
            let speaker = format!("chrg_speaker:{}", &text::sha_hex(format!("{pkg}\0{}", turn.label.to_lowercase()).as_bytes())[..16]);
            let url = format!("https://www.govinfo.gov/content/pkg/{pkg}/html/{pkg}.htm");
            let doc = Doc {
                id: format!("pool-chrg-{pkg}-t{i}"),
                collection: COLLECTION,
                domain: "government_speech",
                source_group: family.clone(),
                writer_key: Some(speaker.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "hearing_speaker_turn", "span_basis": "turn_paragraph_index_half_open", "turn_index": i,
                             "label_removed": true, "reaction_markers_removed_in_turn": turn.markers_removed,
                             "insertions_cut_in_turn": turn.insertions_cut, "file_sha256": sha, "ocr": ocr.to_value(),
                             "unit_note": "one speaker turn of the stenographic hearing record; whole GPO paragraphs"}),
                provenance: json!({"source_url": url, "package": pkg, "hearing_date": date, "speaker_key": speaker,
                                   "speaker_label_sha256": text::sha_hex(turn.label.as_bytes()), "group_key": family,
                                   "group_basis": "hearing", "cohort_id": COHORT}),
                date: Some(date.clone()),
                date_basis: "latest_hearing_date_printed_in_front_matter",
                cutoff_policy: "dated_before_cutoff",
                rights: legal::public_domain_rights(
                    LICENSE,
                    format!("Congressional hearing {pkg}, {date}; GovInfo, US Government Publishing Office; {url}"),
                    "https://www.govinfo.gov/help/chrg",
                    "US government work (17 U.S.C. 105). Spoken record only: prepared statements, submissions and appendices, which may carry third-party rights, are excluded.",
                    COHORT,
                ),
                aliases: vec![],
            };
            if run.accept(&unit, &doc)? {
                markers += turn.markers_removed as u64;
                soft.push(ocr.soft_per_100());
            }
        }
    }
    soft.sort_by(f64::total_cmp);
    Ok(json!({"hearings_rendered": hearings, "reaction_markers_removed_in_admitted_turns": markers,
              "ocr_screen": {"method": legal::OCR_SCREEN_METHOD, "admitted_soft_per_100_words_p90": soft.get(soft.len().saturating_sub(1) * 9 / 10)}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(s: &str) -> String {
        let mut out = String::from("    ");
        let mut col = 4;
        for w in s.split_whitespace() {
            if col + w.len() > 64 {
                out.push('\n');
                col = 0;
            } else if col > 4 {
                out.push(' ');
                col += 1;
            }
            out.push_str(w);
            col += w.len();
        }
        out.push('\n');
        out
    }

    #[test]
    fn turns_split_at_labels_and_drop_insertions() {
        let long = vec!["We have heard the testimony and we will consider it carefully."; 4].join(" ");
        let pre = format!(
            "\n                       THURSDAY, JANUARY 28, 2010\n\n                    OPENING STATEMENT OF HON. JANE DOE\n\n{}{}{}{}{}{}\n                               A P P E N D I X\n\n{}",
            wrap(&format!("The Chairman. {long} [Laughter]")),
            wrap(&long),
            wrap(&format!("Mr. Roe. {long}")),
            wrap("[The prepared statement of Mr. Roe follows:]"),
            wrap(&format!("{long} Written words that never were spoken aloud here.")),
            wrap(&format!("Senator Smith. {long}")),
            wrap(&format!("Mr. Late. {long}"))
        );
        let (ts, front) = turns(&pre);
        assert_eq!(dates_in(&front), vec!["2010-01-28"]);
        assert_eq!(ts.len(), 3);
        assert_eq!(ts[0].label, "The Chairman");
        assert_eq!(ts[0].markers_removed, 1);
        assert_eq!(ts[1].label, "Mr. Roe");
        assert_eq!(ts[1].insertions_cut, 1);
        assert!(ts[1].paras.iter().all(|p| !p.text.contains("Written words")));
        assert_eq!(ts[2].label, "Senator Smith");
        assert!(ts[0].paras[0].text.starts_with("We have heard"));
        assert_eq!(turn_label("Mr. Bordeaux. Good afternoon."), Some(("Mr. Bordeaux", "Good afternoon.")));
        assert_eq!(turn_label("The Committee met, pursuant to notice. It was late."), None);
        assert_eq!(turn_label("Ms. Sparks' work on these issues. More."), None);
        assert_eq!(turn_label(". Leading full stop. Then text."), None);
        let multibyte = format!("Mr. {}é. tail", "a".repeat(76));
        let _ = turn_label(&multibyte);
    }

    #[test]
    fn hearing_file_produces_turn_documents() {
        let para = vec!["We have heard the testimony and we will consider it carefully today."; 8].join(" ");
        let body = format!("\n        JANUARY 28, 2010\n\n{}{}{}", wrap(&format!("The Chairman. {para}")), wrap(&para), wrap("Mr. Roe. Thank you very much."));
        let html = format!("<html><body><pre>{body}</pre></body></html>");
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("CHRG-111shrg00001.htm");
        std::fs::write(&f, html).unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "govinfo-hearings", COLLECTION, json!({}), None).unwrap();
        adapt(&[f], &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 1);
        let rows: Vec<Value> = crate::io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["source_group"], "govinfo_chrg:CHRG-111shrg00001");
        assert!(!rows[0]["text"].as_str().unwrap().contains("Chairman"));
        assert_eq!(rows[0]["date"]["value"], "2010-01-28");
    }
}

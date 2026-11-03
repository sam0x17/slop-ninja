//! Library of Congress By the People (Concordia) transcription datasets:
//! volunteer transcriptions of historical letters, diaries, speeches and
//! records, one CSV row per page image, inside the per-campaign dataset ZIPs
//! of the Selected Datasets collection.
//!
//! * **Human transcription, dated.** A campaign manifest
//!   (`btp-campaigns-v1.json`) lists each admitted dataset ZIP by SHA-256 and,
//!   per campaign, the README sentence that states when the campaign was
//!   fully transcribed and reviewed. Only campaigns whose transcription and
//!   review were completed before 2022-11-30 are admitted; the adapter
//!   verifies every evidence sentence verbatim (whitespace-normalised) in the
//!   ZIP's README. Rows of any other campaign are rejected.
//! * **Rights.** The transcriptions are released into the public domain as
//!   they are created (dataset README). The underlying documents follow the
//!   Library's item and collection rights statements, which rows record.
//!   Rows also carry the classifier-training basis (user decision
//!   2026-10-27); their text is kept in ignored storage and never
//!   redistributed.
//! * **Groups.** Unit and source family: one catalogue item
//!   (`loc_btp_item:<ItemId>`), usually one letter, speech or diary volume.
//!   Bucket, split group and `writer_key`: the manifest's collection key
//!   (`loc_btp_collection:<key>`), the scout's collection-level grouping;
//!   correspondents are not separated.
//! * **Markup** (`btp_markup_v2`). Marginalia `[*...*]`, brackets holding
//!   `?` or "illegible", unbalanced brackets and `?` illegibility marks
//!   (inside a word or as a bare token) and bracketed editorial notes about
//!   the page (`EDITORIAL_WORDS`: torn, blank, drawing, signature, ...)
//!   and parenthesised all-capitals transcriber notes (`(LAUGHS)`) become gaps, so their paragraph is
//!   never admitted. Other square-bracketed text is the writer's deleted text
//!   and is removed (the final text is kept). Lines holding only a page
//!   number, and `--N--` page marks inside a line, are dropped. Paragraphs
//!   with two or more transcript speaker labels (`AL:`) are not admitted.
//!   Every case is counted per document.
//! * **Paragraphs.** Volunteers keep the manuscript's line breaks, so blank
//!   lines separate paragraph blocks. Short lines (under five words) that
//!   open a block before a capitalised line (headings, dates, salutations)
//!   or close it after a sentence end (closings, signatures) are split off
//!   as non-prose lines. The remaining lines are unwrapped with the OCR
//!   rules (`ocr`). A block (on the same or the next page) that starts in
//!   lowercase after a block without a sentence end continues it; a block
//!   still starting in lowercase is never an excerpt start. A page that
//!   is not completed is a break. Up to `max_runs` successive
//!   whole-paragraph runs of 150..450 words are taken per item.
use crate::{
    adapter::Run,
    ocr::{self, Lexicon, Normalisation, Quality, QualityStats},
    record::{self, Doc},
    text::{self, GAP, Para, Screen},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::Path};

pub const COLLECTION: &str = "loc_by_the_people_historical";
pub const LICENSE: &str = "public-domain-volunteer-transcription;underlying-loc-collection-rights";
pub const MARKUP_METHOD: &str = "btp_markup_v2";
pub const SHORT_LINE_WORDS: usize = 5;
/// Words that mark a bracketed span as an editorial note about the page
/// (not the writer's deleted text); such a span becomes a gap.
pub const EDITORIAL_WORDS: &[&str] = &[
    "torn", "blank", "drawing", "drawn", "image", "illustration", "picture", "photo", "photograph", "stamp", "stamped", "seal", "signature",
    "signed", "missing", "unclear", "faded", "cut", "page", "written", "printed", "handwritten", "typed", "typewritten", "struck", "crossed",
    "margin", "marginalia", "text", "continued", "over", "reverse", "verso", "enclosure", "envelope", "letterhead", "postmark", "header",
    "footer", "insert", "inserted", "inserting", "underlined", "underline", "deleted", "deletion", "unintelligible", "indecipherable", "damaged",
    "smudged", "symbol", "sketch", "map", "table", "chart", "number", "numbers", "column", "columns", "music", "notation", "shorthand", "end",
];

fn editorial(inner: &str) -> bool {
    text::folded_tokens(inner).iter().any(|w| EDITORIAL_WORDS.contains(&w.as_str()))
}

/// RFC 4180 CSV: quoted fields may hold separators, quotes (doubled) and
/// line breaks. A leading byte-order mark is skipped.
pub fn parse_csv(s: &str) -> Vec<Vec<String>> {
    let s = s.strip_prefix('\u{FEFF}').unwrap_or(s);
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                any = false;
            }
            _ => field.push(c),
        }
    }
    if any {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Markup {
    pub marginalia_gaps: u64,
    pub illegible_bracket_gaps: u64,
    pub unbalanced_bracket_gaps: u64,
    pub question_mark_gaps: u64,
    pub deletions_removed: u64,
    pub page_number_lines_dropped: u64,
    pub short_lines_split: u64,
    pub page_continuations_joined: u64,
    pub incomplete_page_breaks: u64,
    pub empty_pages: u64,
    pub stage_note_gaps: u64,
}

impl Markup {
    pub fn add(&mut self, o: &Markup) {
        self.marginalia_gaps += o.marginalia_gaps;
        self.illegible_bracket_gaps += o.illegible_bracket_gaps;
        self.unbalanced_bracket_gaps += o.unbalanced_bracket_gaps;
        self.question_mark_gaps += o.question_mark_gaps;
        self.deletions_removed += o.deletions_removed;
        self.page_number_lines_dropped += o.page_number_lines_dropped;
        self.short_lines_split += o.short_lines_split;
        self.page_continuations_joined += o.page_continuations_joined;
        self.incomplete_page_breaks += o.incomplete_page_breaks;
        self.empty_pages += o.empty_pages;
        self.stage_note_gaps += o.stage_note_gaps;
    }
    pub fn to_value(&self) -> Value {
        json!({"method": MARKUP_METHOD, "marginalia_gaps": self.marginalia_gaps, "illegible_bracket_gaps": self.illegible_bracket_gaps,
               "unbalanced_bracket_gaps": self.unbalanced_bracket_gaps, "question_mark_gaps": self.question_mark_gaps,
               "deletions_removed": self.deletions_removed, "page_number_lines_dropped": self.page_number_lines_dropped,
               "short_lines_split": self.short_lines_split, "page_continuations_joined": self.page_continuations_joined,
               "incomplete_page_breaks": self.incomplete_page_breaks, "empty_pages": self.empty_pages,
               "stage_note_gaps": self.stage_note_gaps})
    }
}

/// `--1--`, `- 2 -` style page marks inside a line.
fn page_mark(tok: &str) -> bool {
    let core = tok.trim_matches('-');
    tok.starts_with('-') && tok.ends_with('-') && !core.is_empty() && core.len() <= 4 && core.bytes().all(|b| b.is_ascii_digit())
}

/// A parenthesised all-capitals transcriber note such as `(LAUGHS)`.
fn stage_note(inner: &str) -> bool {
    let letters = inner.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 3 && inner.chars().all(|c| c.is_ascii_uppercase() || c == ' ' || c == '-' || c == '.' || c == ',')
}

/// Apply the bracket, transcriber-note and `?` rules to one line.
pub fn line_markup(line: &str, m: &mut Markup) -> String {
    let toks: Vec<&str> = line.split(' ').collect();
    let kept: Vec<&str> = toks.iter().copied().filter(|t| !page_mark(t)).collect();
    m.page_number_lines_dropped += (toks.len() - kept.len()) as u64;
    let line = kept.join(" ");
    // Transcriber notes in parentheses.
    let mut noted = String::with_capacity(line.len());
    let mut r = line.as_str();
    while let Some(i) = r.find('(') {
        let after = &r[i + 1..];
        match after.find(')') {
            Some(j) if stage_note(&after[..j]) => {
                noted.push_str(&r[..i]);
                noted.push(GAP);
                m.stage_note_gaps += 1;
                r = &after[j + 1..];
            }
            _ => {
                noted.push_str(&r[..=i]);
                r = after;
            }
        }
    }
    noted.push_str(r);
    let mut out = String::with_capacity(noted.len());
    let mut rest = noted.as_str();
    while let Some(i) = rest.find(['[', ']']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with(']') {
            out.push(GAP);
            m.unbalanced_bracket_gaps += 1;
            rest = &tail[1..];
            continue;
        }
        if let Some(inner_tail) = tail.strip_prefix("[*") {
            if let Some(j) = inner_tail.find("*]") {
                out.push(GAP);
                m.marginalia_gaps += 1;
                rest = &inner_tail[j + 2..];
                continue;
            }
        }
        let after = &tail[1..];
        match after.find(['[', ']']) {
            Some(j) if after.as_bytes()[j] == b']' => {
                let inner = &after[..j];
                if inner.contains('?') || inner.to_lowercase().contains("illegible") || inner.starts_with('*') || editorial(inner) {
                    out.push(GAP);
                    m.illegible_bracket_gaps += 1;
                } else {
                    m.deletions_removed += 1;
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push(GAP);
                m.unbalanced_bracket_gaps += 1;
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // `?` illegibility marks: inside a word, or a bare token of question marks.
    let chars: Vec<char> = out.chars().collect();
    let mut res = String::with_capacity(out.len());
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c != '?' {
            res.push(c);
            k += 1;
            continue;
        }
        let mut e = k;
        while e < chars.len() && chars[e] == '?' {
            e += 1;
        }
        let before = k.checked_sub(1).map(|p| chars[p]);
        let after = chars.get(e).copied();
        let inside_word = before.is_some_and(char::is_alphanumeric) && after.is_some_and(char::is_alphanumeric);
        let bare = before.is_none_or(char::is_whitespace) && after.is_none_or(|a| a.is_whitespace() || ".,;:".contains(a));
        if inside_word || bare {
            res.push(GAP);
            m.question_mark_gaps += 1;
        } else {
            res.extend(&chars[k..e]);
        }
        k = e;
    }
    res
}

fn page_number_line(l: &str) -> bool {
    let t = l.trim().trim_matches(|c: char| "[]().-".contains(c)).trim();
    let t = t.strip_prefix("Page ").or_else(|| t.strip_prefix("page ")).or_else(|| t.strip_prefix("p. ")).unwrap_or(t);
    !t.is_empty() && t.len() <= 4 && t.chars().all(|c| c.is_ascii_digit())
}

/// One paragraph block of a page before classification.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub text: String,
    /// Split-off heading, salutation, closing or signature line.
    pub detached: bool,
}

fn ends_clause(s: &str) -> bool {
    let t = s.trim_end().trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')']);
    t.ends_with(['.', '!', '?', ':', ';'])
}

/// Blocks of one page transcription (markup applied, lines unwrapped).
pub fn page_blocks(t: &str, lex: &Lexicon, m: &mut Markup, n: &mut Normalisation) -> Vec<Block> {
    let t = ocr::map_chars(&t.replace("\r\n", "\n"), n);
    if t.trim().is_empty() || t.trim().eq_ignore_ascii_case("nothing to transcribe") {
        m.empty_pages += 1;
        return vec![];
    }
    // Line-level markup; a line emptied by removal is dropped (not blank).
    let mut lines: Vec<String> = Vec::new();
    for l in t.split('\n') {
        if l.trim().is_empty() {
            lines.push(String::new());
            continue;
        }
        if page_number_line(l) {
            m.page_number_lines_dropped += 1;
            continue;
        }
        let x = line_markup(l, m);
        if !x.trim().is_empty() {
            lines.push(x);
        }
    }
    let joined = lines.join("\n");
    let mut out = Vec::new();
    for b in ocr::blocks(&joined) {
        let short = |l: &str| text::words(l) < SHORT_LINE_WORDS;
        let mut lo = 0;
        while lo + 1 < b.len() && short(b[lo]) && b[lo + 1].trim_start().chars().next().is_some_and(char::is_uppercase) {
            lo += 1;
        }
        // Closing: the longest suffix of short lines that follows a clause end.
        let mut hi = b.len();
        let mut s = b.len();
        while s > lo + 1 && short(b[s - 1]) {
            s -= 1;
            if ends_clause(b[s - 1]) {
                hi = s;
            }
        }
        for l in &b[..lo] {
            out.push(Block { text: text::squash(l), detached: true });
            m.short_lines_split += 1;
        }
        out.push(Block { text: ocr::unwrap_lines(&b[lo..hi], lex, n), detached: false });
        for l in &b[hi..] {
            out.push(Block { text: text::squash(l), detached: true });
            m.short_lines_split += 1;
        }
    }
    out
}

/// Join one item's pages into classified paragraphs. `None` pages are not
/// completed and break runs.
pub fn item_paragraphs(pages: &[Option<&str>], lex: &Lexicon, m: &mut Markup, n: &mut Normalisation) -> Vec<Para> {
    let mut blocks: Vec<Block> = Vec::new();
    for p in pages {
        let Some(t) = p else {
            blocks.push(Block { text: GAP.to_string(), detached: true });
            m.incomplete_page_breaks += 1;
            continue;
        };
        for (i, b) in page_blocks(t, lex, m, n).into_iter().enumerate() {
            let open = blocks.last().is_some_and(|x| !x.detached && !text::ends_sentence(&x.text) && !x.text.contains(GAP));
            if open && !b.detached && b.text.chars().next().is_some_and(char::is_lowercase) {
                let last = blocks.last_mut().expect("open block");
                last.text = ocr::unwrap_lines(&[last.text.as_str(), b.text.as_str()], lex, n);
                if i == 0 {
                    m.page_continuations_joined += 1;
                } else {
                    n.block_continuations_joined += 1;
                }
                continue;
            }
            blocks.push(b);
        }
    }
    blocks
        .into_iter()
        .map(|b| {
            if b.detached {
                Para::non_prose(b.text, "detached_short_line")
            } else if speaker_labels(&b.text) >= 2 {
                Para::non_prose(b.text, "speaker_labelled_transcript")
            } else {
                ocr::classify_block(b.text, Screen::XML)
            }
        })
        .collect()
}

/// Count of transcript speaker labels (`AL:`, `MR. X:` style: one to four
/// capitals then a colon) at the start of the text or after a sentence end.
pub fn speaker_labels(t: &str) -> usize {
    let w: Vec<&str> = t.split_whitespace().collect();
    (0..w.len())
        .filter(|&i| {
            let x = w[i];
            let core = x.strip_suffix(':').unwrap_or("");
            !core.is_empty() && core.len() <= 4 && core.chars().all(|c| c.is_ascii_uppercase())
                && (i == 0 || text::ends_sentence(w[i - 1]) || w[i - 1].ends_with(['-', '\u{2014}']))
        })
        .count()
}

/// Up to `max_runs` successive non-overlapping runs (spans are paragraph
/// indexes into `paras`).
pub fn runs(paras: &[Para], max_runs: usize) -> Vec<text::Excerpt> {
    let mut out = Vec::new();
    let mut start = 0;
    while out.len() < max_runs && start < paras.len() {
        let Some(mut ex) = text::paragraph_run(&paras[start..]) else { break };
        ex.start += start;
        ex.end += start;
        ex.complete_unit = ex.complete_unit && start == 0;
        start = ex.end;
        out.push(ex);
    }
    out
}

pub struct Campaign {
    pub name: String,
    pub group: String,
    pub completed_by: String,
    pub evidence: Vec<String>,
}

pub struct Dataset {
    pub zip: String,
    pub zip_sha256: String,
    pub item_url: String,
    pub campaigns: Vec<Campaign>,
}

pub fn parse_manifest(v: &Value) -> Result<Vec<Dataset>> {
    ensure!(v["schema"] == "slop_ninja_a_pool_btp_campaigns_v1", "manifest schema");
    let mut out = Vec::new();
    for d in v["datasets"].as_array().context("datasets")? {
        let mut cs = Vec::new();
        for c in d["campaigns"].as_array().context("campaigns")? {
            let completed_by = c["completed_by"].as_str().context("completed_by")?.to_owned();
            ensure!(record::valid_date(&completed_by), "completed_by date");
            cs.push(Campaign {
                name: c["name"].as_str().context("name")?.to_owned(),
                group: c["group"].as_str().context("group")?.to_owned(),
                completed_by,
                evidence: c["evidence"].as_array().context("evidence")?.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            });
        }
        out.push(Dataset {
            zip: d["zip"].as_str().context("zip")?.to_owned(),
            zip_sha256: d["zip_sha256"].as_str().context("zip_sha256")?.to_owned(),
            item_url: d["item_url"].as_str().context("item_url")?.to_owned(),
            campaigns: cs,
        });
    }
    Ok(out)
}

fn asset_seq(asset: &str) -> Option<u64> {
    asset.rsplit('-').next()?.parse().ok()
}

pub struct Options<'a> {
    pub manifest: &'a Path,
    pub datasets_dir: &'a Path,
    pub lexicon: &'a Lexicon,
    pub max_runs: usize,
}

pub fn adapt(opt: &Options, run: &mut Run) -> Result<Value> {
    let (msha, _) = crate::io::file_sha(opt.manifest)?;
    let datasets = parse_manifest(&serde_json::from_slice(&std::fs::read(opt.manifest)?)?)?;
    let method = format!("btp_item_pages;{MARKUP_METHOD};{};{}", ocr::NORMALISATION_METHOD, text::PARAGRAPH_RUN_METHOD);
    let mut per_ds = Vec::new();
    let mut stats = QualityStats::default();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut markup_total = Markup::default();
    let mut norm_total = Normalisation::default();
    for ds in &datasets {
        let path = opt.datasets_dir.join(&ds.zip);
        let (sha, bytes) = crate::io::file_sha(&path)?;
        ensure!(sha == ds.zip_sha256, "{}: SHA-256 {} does not match the manifest", ds.zip, sha);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
        let mut readme = String::new();
        let mut csv_name = None;
        for i in 0..z.len() {
            let name = z.by_index(i)?.name().to_owned();
            let lower = name.to_lowercase();
            if lower.contains("readme") {
                z.by_index(i)?.read_to_string(&mut readme)?;
            } else if lower.ends_with(".csv") {
                ensure!(csv_name.is_none(), "{}: more than one CSV", ds.zip);
                csv_name = Some(name);
            }
        }
        let csv_name = csv_name.with_context(|| format!("{}: no CSV", ds.zip))?;
        let readme_norm = text::normalized(&readme);
        for c in &ds.campaigns {
            ensure!(record::before_cutoff(&c.completed_by), "{}: campaign {} completed {} is not before the cutoff", ds.zip, c.name, c.completed_by);
            for e in &c.evidence {
                ensure!(readme_norm.contains(&text::normalized(e)), "{}: evidence sentence not found in README: {e}", ds.zip);
            }
        }
        let mut body = String::new();
        z.by_name(&csv_name)?.read_to_string(&mut body)?;
        let rows = parse_csv(&body);
        ensure!(!rows.is_empty(), "{}: empty CSV", ds.zip);
        let head = &rows[0];
        let col = |n: &str| head.iter().position(|h| h.trim() == n).with_context(|| format!("{}: no column {n}", ds.zip));
        let (ci, cii, ca, cs, ct, cc, cp) = (col("Item")?, col("ItemId")?, col("Asset")?, col("AssetStatus")?, col("Transcription")?, col("Campaign")?, col("Project")?);
        let cu = col("DownloadUrl").ok();
        // Items in first-appearance order; pages by asset sequence.
        let mut order: Vec<String> = Vec::new();
        let mut items: BTreeMap<String, Vec<&Vec<String>>> = BTreeMap::new();
        for r in &rows[1..] {
            if r.len() < head.len() {
                run.count("short_csv_row");
                continue;
            }
            let id = r[cii].clone();
            if !items.contains_key(&id) {
                order.push(id.clone());
            }
            items.entry(id).or_default().push(r);
        }
        let mut ds_counts: BTreeMap<&'static str, u64> = BTreeMap::new();
        for id in &order {
            let mut pages = items[id].clone();
            pages.sort_by_key(|r| asset_seq(&r[ca]).unwrap_or(u64::MAX));
            run.units_seen += 1;
            let unit = format!("loc_btp_item:{id}");
            let campaign_name = pages[0][cc].clone();
            let Some(c) = ds.campaigns.iter().find(|c| c.name == campaign_name) else {
                *ds_counts.entry("item_campaign_not_in_manifest").or_default() += 1;
                run.reject(&unit, "campaign_transcription_period_not_documented_before_cutoff")?;
                continue;
            };
            if pages.iter().any(|r| r[cc] != campaign_name) {
                run.reject(&unit, "item_spans_campaigns")?;
                continue;
            }
            let group = format!("loc_btp_collection:{}", c.group);
            let title = pages[0][ci].clone();
            let project = pages[0][cp].clone();
            let completed = pages.iter().filter(|r| r[cs] == "completed").count();
            let meta = || {
                json!({"item_id": id, "dataset_zip": ds.zip, "campaign": campaign_name, "project": project, "collection_group": c.group,
                       "item_title_sha256": text::sha_hex(title.as_bytes()), "assets": pages.len(), "completed_assets": completed})
            };
            if !run.sampled_meta_grouped(&group, &unit, meta)? {
                continue;
            }
            let texts: Vec<Option<&str>> = pages.iter().map(|r| (r[cs] == "completed").then_some(r[ct].as_str())).collect();
            let mut m = Markup::default();
            let mut n = Normalisation::default();
            let paras = item_paragraphs(&texts, opt.lexicon, &mut m, &mut n);
            for x in &paras {
                if let Some(r) = x.reason {
                    *breaks.entry(r).or_default() += 1;
                }
            }
            let exs = runs(&paras, opt.max_runs);
            if exs.is_empty() {
                run.reject(&unit, "no_paragraph_run_within_bounds")?;
                continue;
            }
            markup_total.add(&m);
            norm_total.add(&n);
            for (k, ex) in exs.iter().enumerate() {
                let rid = format!("{unit}#run{k}");
                if let Some(r) = text::english_screen(&ex.text) {
                    run.reject(&rid, r)?;
                    continue;
                }
                let q = Quality::measure(&ex.text, opt.lexicon);
                stats.add(&q);
                let doc = Doc {
                    id: format!("pool-btp-{id}-r{k}"),
                    collection: COLLECTION,
                    domain: "letters_diaries_manuscripts",
                    source_group: unit.clone(),
                    writer_key: Some(group.clone()),
                    evidence: "historical_public_domain_proxy",
                    excerpt: ex,
                    extraction_method: &method,
                    unit: json!({"kind": "btp_item_page_transcriptions", "span_basis": "item_paragraph_index_half_open", "run_index": k,
                                 "assets": pages.len(), "completed_assets": completed,
                                 "asset_transcription_sha256": pages.iter().map(|r| text::sha_hex(r[ct].as_bytes())).collect::<Vec<_>>(),
                                 "dataset_zip": ds.zip, "dataset_zip_sha256": sha, "csv": csv_name, "markup": m.to_value(),
                                 "normalisation": n.to_value(), "transcription_quality": q.to_value(), "ocr_text": false,
                                 "transcription_markup_normalised": true}),
                    provenance: json!({"campaign": campaign_name, "project": project, "item": title, "item_id": id,
                                       "assets": pages.iter().map(|r| r[ca].clone()).collect::<Vec<_>>(),
                                       "image_urls": cu.map(|u| pages.iter().map(|r| r[u].clone()).collect::<Vec<_>>()),
                                       "dataset_item_url": ds.item_url, "collection_group": c.group,
                                       "transcription_completed_by": c.completed_by,
                                       "transcription_basis": "By the People volunteers; at least one reviewer accepted each page as complete (dataset README)",
                                       "composition_date": "historical; see the item title and collection record",
                                       "source_url": format!("https://www.loc.gov/item/{}/", id.trim_start_matches("mss"))}),
                    date: Some(c.completed_by.clone()),
                    date_basis: "btp_campaign_transcription_and_review_completed_by_per_dataset_readme;original_writing_historical",
                    cutoff_policy: "dated_before_cutoff",
                    rights: json!({
                        "license": LICENSE,
                        "license_url": null,
                        "attribution": format!("{title}; By the People transcription ({campaign_name}), Library of Congress, {}; volunteer transcription released into the public domain; markup normalised", ds.item_url),
                        "evidence_url": ds.item_url,
                        "commercial_training": true,
                        "model_release": true,
                        "external_evaluation": true,
                        "redistribute_text": false,
                        "admission_decision": "historical_2026-10-27: By the People transcriptions completed and reviewed before 2022-11-30; transcriptions public domain; underlying documents under the Library's collection rights statements; classifier-training use, user decision 2026-10-27; text in ignored storage, never redistributed",
                        "basis": "classifier-training use, user decision 2026-10-27",
                        "notes": "Some 20th-century writings in these collections may remain in copyright or carry privacy and publicity considerations; the Library's item and collection rights statements apply.",
                    }),
                    aliases: vec![],
                };
                run.accept(&rid, &doc)?;
            }
        }
        let d = json!({"role": "btp_dataset_zip", "zip": ds.zip, "path": path, "sha256": sha, "bytes": bytes, "csv": csv_name, "csv_rows": rows.len() - 1,
                       "items": order.len(), "readme_sha256": text::sha_hex(readme.as_bytes()), "item_url": ds.item_url,
                       "campaigns": ds.campaigns.iter().map(|c| json!({"name": c.name, "group": c.group, "completed_by": c.completed_by, "evidence": c.evidence})).collect::<Vec<_>>(),
                       "counts": ds_counts});
        run.input(d.clone());
        per_ds.push(d);
    }
    Ok(json!({"manifest_sha256": msha, "datasets": per_ds, "paragraph_breaks": breaks, "markup_totals_screened_items": markup_total.to_value(),
              "normalisation_totals_screened_items": norm_total.to_value(), "transcription_quality_admitted_runs": stats.to_value(),
              "lexicon_sha256": opt.lexicon.sha256, "max_runs_per_item": opt.max_runs,
              "grouping": "unit and source family: catalogue item; bucket, split group and writer_key: collection key from the campaign manifest",
              "limits": ["writers within a collection (the namesake and correspondents) are not separated",
                         "transcriptions follow the manuscript spelling; the lexicon rate measures spelling and transcription, not OCR",
                         "composition dates are historical but not parsed per item"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lex() -> Lexicon {
        Lexicon::from_words(["remember", "the", "day"])
    }
    #[test]
    fn csv_quotes_and_newlines() {
        let r = parse_csv("\u{FEFF}A,B,C\r\n1,\"two, \"\"quoted\"\"\nline\",3\r\n4,,\n");
        assert_eq!(r, vec![vec!["A", "B", "C"], vec!["1", "two, \"quoted\"\nline", "3"], vec!["4", "", ""]]);
    }
    #[test]
    fn markup_rules() {
        let mut m = Markup::default();
        assert_eq!(line_markup("I went [went] home", &mut m), "I went  home");
        assert_eq!(m.deletions_removed, 1);
        let g = line_markup("we saw [*in margin*] it and [?] then w?th ?? him", &mut m);
        assert_eq!(g.matches(GAP).count(), 4);
        assert_eq!((m.marginalia_gaps, m.illegible_bracket_gaps, m.question_mark_gaps), (1, 1, 2));
        assert_eq!(line_markup("Did you go? Yes.", &mut m), "Did you go? Yes.");
        assert!(line_markup("an open [bracket", &mut m).contains(GAP));
        assert!(line_markup("the [page torn] rest", &mut m).contains(GAP));
        assert_eq!(line_markup("I [went] go", &mut m), "I  go");
        assert_eq!(line_markup("--1-- power in the state", &mut m), "power in the state");
        assert!(line_markup("ah, (MUMBLES), I don't know (but I think so)", &mut m).contains(GAP));
        assert!(line_markup("I think (but am not sure) so", &mut m).find(GAP).is_none());
        assert_eq!(speaker_labels("AL: Did she tell you? TL: I don't think so. AL: No."), 3);
        assert_eq!(speaker_labels("He wrote: I went home. Then NOTE: nothing"), 0);
    }
    #[test]
    fn letter_pages_join_and_detach() {
        let mut m = Markup::default();
        let mut n = Normalisation::default();
        let p1 = "Washington Jan 4 1863\nDear Mother\nI take my pen in hand to tell you that we are\nall well and I hope you will remem-\nber the";
        let p2 = "2\nday we left home and the long road we took.\nYour loving son,\nJohn";
        let paras = item_paragraphs(&[Some(p1), Some(p2)], &lex(), &mut m, &mut n);
        let texts: Vec<&str> = paras.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["Washington Jan 4 1863", "Dear Mother",
            "I take my pen in hand to tell you that we are all well and I hope you will remember the day we left home and the long road we took.",
            "Your loving son,", "John"]);
        assert!(paras[2].prose && !paras[0].prose && !paras[4].prose);
        assert_eq!((m.page_number_lines_dropped, m.page_continuations_joined, m.short_lines_split), (1, 1, 4));
        let gap = item_paragraphs(&[Some(p1), None, Some(p2)], &lex(), &mut m, &mut n);
        assert!(gap.iter().any(|p| p.text.contains(GAP)));
        let s = |k: usize| Para::classify(format!("{}.", vec!["word"; k].join(" ")), Screen::XML);
        let ps = vec![s(230), Para::non_prose("x", "t"), s(200), s(30), s(500)];
        let r = runs(&ps, 3);
        assert_eq!(r.iter().map(|e| (e.start, e.end)).collect::<Vec<_>>(), vec![(0, 1), (2, 4)]);
    }
}

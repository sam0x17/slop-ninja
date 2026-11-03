//! Human components of the Pile, read from their original releases so the
//! writer metadata the Pile dropped is available for grouping:
//!
//! * **Enron emails** (CMU 2015-05-07 release of the FERC public record):
//!   only messages in the mailbox owner's own sent folders whose `From`
//!   address names the owner; quoted, forwarded and header material is cut
//!   before paragraphs are taken. Bucket, split group and `writer_key`: the
//!   mailbox owner (opaque key); source family: the message.
//! * **EuroParl v7 English** (statmt source release): speaker turns of the
//!   English files whose speaker carries no `LANGUAGE` attribute other than
//!   `EN` in a file that marks original languages, so translated speeches
//!   are excluded; chair turns are skipped. Group: speaker name (opaque).
//! * **NIH ExPORTER abstracts** (fiscal-year project and abstract CSVs):
//!   the applicant's description section, label prefixes removed and the
//!   public-health-relevance or narrative section cut. Group: contact PI
//!   (opaque); source family: core project number.
//!
//! USPTO backgrounds are not read from HUPD: its card states CC BY-NC-SA 4.0
//! in its licensing section, and a non-commercial term excludes a source.
//!
//! Every rendered unit passes the shared paragraph run (150..=450
//! whitespace words, whole paragraphs) and the English screen.
use crate::{
    adapter::Run,
    forum,
    io::HashingReader,
    record::{self, Doc},
    text::{self, Para, Screen},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    io::Read,
    path::Path,
};

pub const ADMISSION_BASIS: &str = forum::ADMISSION_BASIS;
pub const CONTRIBUTOR_DOMAIN: &str = "slop_ninja_a_pile_contributor_v1";

pub const ENRON_COLLECTION: &str = "enron_email_historical";
pub const EUROPARL_COLLECTION: &str = "europarl_en_original_historical";
pub const NIH_COLLECTION: &str = "nih_exporter_abstract_historical";

/// Enron mail: a public record released by FERC during its investigation;
/// the CMU distribution states no licence.
pub const ENRON_LICENSE: &str = "ferc-public-record-enron-email;cmu-distribution-no-licence;email-text-unlicensed";
/// European Parliament: reuse, including commercial, authorised with source
/// acknowledgement (EP legal notice); statmt notes no known restrictions.
pub const EUROPARL_LICENSE: &str = "european-parliament-reuse-authorised-source-acknowledged";
/// NIH RePORTER ExPORTER: public federal database; abstracts are written
/// by applicants and carry no licence statement.
pub const NIH_LICENSE: &str = "nih-exporter-public-federal-database;abstract-text-unlicensed";

/// Licence ids this module adds to the admitted set.
pub const TERMS: &[&str] = &[ENRON_LICENSE, EUROPARL_LICENSE, NIH_LICENSE];

/// Opaque contributor key: never the raw name or identifier.
pub fn contributor_key(prefix: &str, raw: &str) -> String {
    let h = text::sha_hex(format!("{CONTRIBUTOR_DOMAIN}\0{prefix}\0{raw}").as_bytes());
    format!("{prefix}:{}", &h[..20])
}

fn pile_rights(license: &str, attribution: String, evidence_url: &str, cohort: &str, notes: &str) -> Value {
    let mut r = record::rights(license, attribution, evidence_url, notes);
    r["admission_decision"] = json!("user_2026-10-28: selected human Pile components from their original releases; no non-commercial terms; classifier-training use, user decision 2026-10-27");
    r["admission_basis"] = json!(ADMISSION_BASIS);
    r["cohort_id"] = json!(cohort);
    r["removable_cohort"] = json!(true);
    r["redistribute_text"] = json!(false);
    r["evaluation_use"] = json!("train_or_evaluation");
    r["pile_component"] = json!(true);
    r
}

fn bump(m: &mut BTreeMap<&'static str, u64>, k: &'static str) {
    *m.entry(k).or_default() += 1;
}

/// Decode bytes as UTF-8; when they are not valid UTF-8, decode line by
/// line, each line as UTF-8 if valid and as Windows-1252 otherwise, so a
/// few legacy-encoded rows do not garble the rest of a file.
pub fn decode_text(b: &[u8]) -> (String, &'static str) {
    match std::str::from_utf8(b) {
        Ok(s) => (s.to_owned(), "utf-8"),
        Err(_) => {
            let mut out = String::with_capacity(b.len());
            for (i, line) in b.split(|&c| c == b'\n').enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                match std::str::from_utf8(line) {
                    Ok(s) => out.push_str(s),
                    Err(_) => out.extend(line.iter().map(|&c| cp1252(c))),
                }
            }
            (out, "utf-8-with-windows-1252-lines")
        }
    }
}

fn cp1252(c: u8) -> char {
    const HI: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š',
        '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    if (0x80..0xA0).contains(&c) { HI[(c - 0x80) as usize] } else { c as char }
}

fn month(m: &str) -> Option<u32> {
    let i = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|x| m.to_lowercase().starts_with(x))?;
    Some(i as u32 + 1)
}

fn iso(y: u32, m: u32, d: u32) -> Option<String> {
    let s = format!("{y:04}-{m:02}-{d:02}");
    ((1..=12).contains(&m) && (1..=31).contains(&d) && record::valid_date(&s)).then_some(s)
}

// ---------------------------------------------------------------- Enron

pub const ENRON_SENT_FOLDERS: &[&str] = &["sent", "sent_items", "_sent_mail"];

/// RFC 822 header block and body (CRLF tolerated).
pub fn split_message(raw: &str) -> (Vec<(String, String)>, String) {
    let raw = raw.replace("\r\n", "\n");
    let (head, body) = match raw.find("\n\n") {
        Some(i) => (&raw[..i], raw[i + 2..].to_owned()),
        None => (raw.as_str(), String::new()),
    };
    let mut h: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(last) = h.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            h.push((k.trim().to_lowercase(), v.trim().to_owned()));
        }
    }
    (h, body)
}

fn header<'a>(h: &'a [(String, String)], k: &str) -> &'a str {
    h.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_str()).unwrap_or("")
}

/// `Thu, 7 Jun 2001 06:51:25 -0700 (PDT)` -> `2001-06-07` (sender local date).
pub fn rfc822_date(s: &str) -> Option<String> {
    let t: Vec<&str> = s.split_whitespace().collect();
    let t: &[&str] = if t.first().is_some_and(|w| w.ends_with(',')) { &t[1..] } else { &t };
    let d: u32 = t.first()?.parse().ok()?;
    let m = month(t.get(1)?)?;
    let y: u32 = t.get(2)?.parse().ok()?;
    iso(y, m, d)
}

/// Lines that begin quoted, forwarded or header material in a reply.
fn reply_marker(line: &str) -> Option<&'static str> {
    let l = line.trim();
    let lower = l.to_lowercase();
    if l.starts_with('>') {
        return Some("quoted_reply");
    }
    if lower.contains("original message") || lower.contains("forwarded by") || lower.starts_with("begin forwarded") {
        return Some("original_or_forwarded_message");
    }
    for p in ["from:", "to:", "sent:", "cc:", "bcc:", "subject:", "date:", "sent by:"] {
        if lower.starts_with(p) {
            return Some("embedded_header");
        }
    }
    if l.starts_with("-----") || l.starts_with("_____") || l.starts_with("*****") || l.starts_with("=====") || l.starts_with("<<") {
        return Some("separator_or_attachment");
    }
    None
}

/// The author's own part of a sent message: everything before the first
/// reply marker, cut back to the start of the paragraph that holds it (so
/// a Lotus Notes name/date block above a `To:` line goes too). Returns the
/// kept text and the marker that ended it.
pub fn own_part(body: &str) -> (String, Option<&'static str>) {
    let lines: Vec<&str> = body.lines().collect();
    let mut cut = lines.len();
    let mut why = None;
    for (i, l) in lines.iter().enumerate() {
        if let Some(r) = reply_marker(l) {
            cut = i;
            why = Some(r);
            break;
        }
    }
    if why.is_some() {
        while cut > 0 && !lines[cut - 1].trim().is_empty() {
            cut -= 1;
        }
    }
    (lines[..cut].join("\n"), why)
}

fn phone_like(t: &str) -> bool {
    let b = t.as_bytes();
    (0..b.len()).any(|i| {
        let d = |j: usize, n: usize| b.get(j..j + n).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
        d(i, 3) && matches!(b.get(i + 3), Some(b'-' | b'.')) && d(i + 4, 4) && !b.get(i + 8).is_some_and(u8::is_ascii_digit) && (i == 0 || !b[i - 1].is_ascii_digit())
    })
}

/// A paragraph re-wrapped after hard wrapping (a long line followed by a
/// one- or two-word line, twice or more): the mark of pasted or forwarded
/// text whose line breaks no longer match the sender's editor.
pub fn rewrapped(t: &str) -> bool {
    let l: Vec<usize> = t.lines().map(text::words).collect();
    (1..l.len().saturating_sub(1)).filter(|&i| l[i] <= 2 && l[i - 1] >= 8).count() >= 2
}

/// Method tag of the lost-character screen below.
pub const LOST_CHARACTER_SCREEN: &str = "lost_character_question_mark_screen_v1";

/// A question mark standing for a character the upstream export could not
/// encode (`Children?s`, `-80?C`, ` ?quoted?`, `ER? binds`): a run of `?`
/// directly followed by a letter or digit, or preceded by the start of the
/// text, whitespace or an opening bracket. In formal registers (`formal`)
/// a run followed by `,;:.` or by a space and a lowercase letter or digit
/// also counts; informal email keeps `ok? i think`. A run that ends a
/// sentence (`this???  We`) is a question mark.
pub fn lost_character_mark(t: &str, formal: bool) -> bool {
    let c: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '?' {
            i += 1;
            continue;
        }
        let start = i;
        while i < c.len() && c[i] == '?' {
            i += 1;
        }
        let prev = start.checked_sub(1).map(|j| c[j]);
        let next = c.get(i).copied();
        if next.is_some_and(char::is_alphanumeric) || prev.is_none_or(|p| p.is_whitespace() || matches!(p, '(' | '[')) {
            return true;
        }
        if formal {
            let after = c.get(i + 1).copied();
            if next.is_some_and(|n| matches!(n, ',' | ';' | ':' | '.')) || (next == Some(' ') && after.is_some_and(|a| a.is_lowercase() || a.is_ascii_digit())) {
                return true;
            }
        }
    }
    false
}

/// Email paragraph screen: the informal forum rule plus breaks for
/// addresses, contact details, quoted-printable residue, tables,
/// re-wrapped pasted text and lost characters.
pub fn email_para(t: String) -> Para {
    let reason = if lost_character_mark(&t, false) {
        Some("lost_character_question_mark")
    } else if t.contains('@') {
        Some("address_or_header_residue")
    } else if phone_like(&t) {
        Some("contact_details")
    } else if t.contains("=20") || t.contains("=09") || t.contains("=3D") || t.lines().any(|l| l.ends_with('=')) {
        Some("quoted_printable_residue")
    } else if t.contains('\t') || t.contains("   ") {
        Some("tabular_or_aligned_text")
    } else if rewrapped(&t) {
        Some("rewrapped_lines_artifact")
    } else {
        forum::informal_reason(&t, Screen::MARKUP)
    };
    Para { prose: reason.is_none(), reason, text: t }
}

pub fn email_paragraphs(own: &str) -> Vec<Para> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let flush = |cur: &mut Vec<&str>, out: &mut Vec<Para>| {
        if !cur.is_empty() {
            out.push(email_para(cur.join("\n")));
            cur.clear();
        }
    };
    for l in own.lines() {
        let t = l.trim();
        if t.is_empty() {
            flush(&mut cur, &mut out);
        } else {
            cur.push(t);
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// Mailbox owner surname from the CMU directory name (`doe-j` -> `doe`).
fn owner_surname(owner: &str) -> &str {
    owner.split('-').next().unwrap_or(owner)
}

/// The From address names the mailbox owner.
pub fn from_owner(from: &str, owner: &str) -> bool {
    let sur = owner_surname(owner).to_lowercase();
    let addr = from.to_lowercase();
    let local = addr.rsplit('<').next().unwrap_or(&addr).split('@').next().unwrap_or("");
    sur.len() >= 2 && local.contains(&sur)
}

pub fn enron(archive: &Path, archive_url: &str, run: &mut Run) -> Result<Value> {
    let method = format!("enron_sent_folder_own_part_v1;email_paragraph_screen_v1;{LOST_CHARACTER_SCREEN};{}", text::PARAGRAPH_RUN_METHOD);
    let f = std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut hr = HashingReader::new(std::io::BufReader::with_capacity(1 << 20, f));
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut cuts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut owners_with: BTreeMap<String, u64> = BTreeMap::new();
    let mut messages = 0u64;
    {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(&mut hr));
        for e in ar.entries()? {
            if run.full() {
                break;
            }
            let mut e = e?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let path = e.path()?.to_string_lossy().into_owned();
            let parts: Vec<&str> = path.split('/').collect();
            if parts.len() != 4 || parts[0] != "maildir" {
                continue;
            }
            messages += 1;
            if !ENRON_SENT_FOLDERS.contains(&parts[2]) {
                run.count("not_owner_sent_folder");
                continue;
            }
            run.units_seen += 1;
            let owner = parts[1].to_owned();
            let owner_key = contributor_key("enron_mailbox", &owner);
            let mut b = Vec::new();
            e.read_to_end(&mut b)?;
            let (raw, enc) = decode_text(&b);
            let (h, body) = split_message(&raw);
            let mid = header(&h, "message-id").to_owned();
            let unit = format!("enron_message:{}", &text::sha_hex(format!("{owner}\0{mid}\0{path}").as_bytes())[..24]);
            let date = rfc822_date(header(&h, "date"));
            let meta = || json!({"archive_path": path, "message_id_sha256": text::sha_hex(mid.as_bytes()), "date": date, "folder": parts[2],
                                 "raw_sha256": text::sha_hex(&b), "owner_key": owner_key});
            if !run.sampled_meta_grouped(&owner_key, &unit, meta)? {
                continue;
            }
            let from = header(&h, "from");
            let ctype = header(&h, "content-type").to_lowercase();
            let reason = if !from_owner(from, &owner) {
                Some("sender_not_mailbox_owner")
            } else if !ctype.is_empty() && !ctype.starts_with("text/plain") {
                Some("not_plain_text")
            } else if date.as_deref().is_none_or(|d| !record::before_cutoff(d) || &d[..4] < "1997") {
                Some("no_valid_date")
            } else {
                None
            };
            if let Some(r) = reason {
                run.reject(&unit, r)?;
                continue;
            }
            let (own, why) = own_part(&body);
            if let Some(w) = why {
                bump(&mut cuts, w);
            }
            let paras = email_paragraphs(&own);
            for p in &paras {
                if let Some(r) = p.reason {
                    bump(&mut breaks, r);
                }
            }
            let Some(ex) = text::paragraph_run(&paras) else {
                run.reject(&unit, "no_paragraph_run_within_bounds")?;
                continue;
            };
            if let Some(r) = text::english_screen(&ex.text) {
                run.reject(&unit, r)?;
                continue;
            }
            let date = date.expect("checked");
            let doc = Doc {
                id: format!("pool-enron-{}", &text::sha_hex(unit.as_bytes())[..24]),
                collection: ENRON_COLLECTION,
                domain: "email_corporate",
                source_group: unit.clone(),
                writer_key: Some(owner_key.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "enron_sent_message_own_part", "archive_path": path, "raw_sha256": text::sha_hex(&b), "decoding": enc,
                             "own_part_end": why, "span_basis": "own_part_paragraph_index_half_open"}),
                provenance: json!({"archive_path": path, "message_id": mid, "folder": parts[2], "date_header": header(&h, "date"),
                                   "subject_sha256": text::sha_hex(header(&h, "subject").as_bytes()), "release": "CMU Enron Email Dataset, May 7, 2015 version",
                                   "source_url": archive_url, "x_origin": header(&h, "x-origin")}),
                date: Some(date.clone()),
                date_basis: "rfc822_date_header_sender_local",
                cutoff_policy: "dated_before_cutoff",
                rights: pile_rights(
                    ENRON_LICENSE,
                    format!("Enron Email Dataset (CMU, May 7, 2015 version), message {path}; sender is the mailbox owner (opaque writer_key)"),
                    "https://www.cs.cmu.edu/~enron/",
                    "pile-enron-v1",
                    "Made public by the US Federal Energy Regulatory Commission during its investigation; CMU distributes the corpus for research and states no licence or use restriction. Personal-data hygiene: owner ids are opaque; text stays in ignored storage.",
                ),
                aliases: vec![],
            };
            if run.accept(&unit, &doc)? {
                *by_year.entry(date[..4].to_owned()).or_default() += 1;
                *owners_with.entry(owner_key.clone()).or_default() += 1;
            }
        }
    }
    std::io::copy(&mut hr, &mut std::io::sink())?;
    let (sha, bytes) = hr.finish();
    Ok(json!({"archive": {"path": archive, "url": archive_url, "sha256": sha, "bytes": bytes}, "messages_in_archive": messages,
              "sent_folders": ENRON_SENT_FOLDERS, "own_part_cut_markers": cuts, "paragraph_breaks": breaks, "candidates_by_year": by_year,
              "owners_with_candidates": owners_with.len(),
              "grouping": "unit and source family: message; bucket, split group and writer_key: mailbox owner (opaque)",
              "limits": ["only the owner's sent folders; the From address must contain the owner's surname, so messages sent by assistants under another address are excluded",
                         "a message an assistant typed from the owner's account cannot be separated",
                         "unmarked forwarded or pasted text inside the owner's part is not detected",
                         "greetings and sign-offs are kept as written; addresses, phone numbers and quoted-printable residue break runs"]}))
}

// ---------------------------------------------------------------- EuroParl

fn attr(tag: &str, name: &str) -> Option<String> {
    let i = tag.find(&format!("{name}="))?;
    let rest = &tag[i + name.len() + 1..];
    if let Some(r) = rest.strip_prefix('"') {
        Some(r[..r.find('"')?].to_owned())
    } else {
        Some(rest.split([' ', '>']).next()?.to_owned())
    }
}

/// Order-free speaker key: first given-name token plus the sorted other
/// tokens, initials and parenthesised groups dropped, so `Evans, Robert J`
/// and `Robert Evans` agree.
pub fn speaker_key(name: &str) -> String {
    let mut n = name.to_owned();
    while let (Some(a), Some(b)) = (n.find('('), n.find(')')) {
        if b <= a {
            break;
        }
        n.replace_range(a..=b, " ");
    }
    let n = n.split(", on behalf").next().unwrap_or(&n).to_owned();
    let (first, rest): (Vec<String>, Vec<String>) = match n.split_once(',') {
        Some((last, given)) => (text::folded_tokens(given), text::folded_tokens(last)),
        None => {
            let t = text::folded_tokens(&n);
            if t.is_empty() { (vec![], vec![]) } else { (vec![t[0].clone()], t[1..].to_vec()) }
        }
    };
    let given = first.iter().find(|t| t.chars().count() > 1).cloned().unwrap_or_default();
    let mut others: Vec<String> = rest.into_iter().filter(|t| t.chars().count() > 1).collect();
    others.sort();
    others.dedup();
    format!("{given}|{}", others.join(" "))
}

pub fn chair(name: &str) -> bool {
    let f = text::folded(name);
    f.is_empty() || f == "president" || f.starts_with("the president") || f == "vice president" || f.starts_with("president ") && f.split(' ').count() <= 2
}

/// Translator-tokenised apostrophes in the older files (`minute' s`).
pub fn apostrophe_artifact(t: &str) -> bool {
    let b = t.as_bytes();
    (1..b.len().saturating_sub(2)).any(|i| b[i] == b'\'' && b[i + 1] == b' ' && b[i - 1].is_ascii_alphabetic() && matches!(b[i + 2], b's' | b't' | b'l' | b'v' | b'r' | b'd' | b'm') && b.get(i + 3).is_none_or(|c| !c.is_ascii_alphabetic()))
}

pub fn europarl_para(line: &str) -> Para {
    let t = line.trim();
    if (t.starts_with('(') && t.ends_with(')')) || (t.starts_with('[') && t.ends_with(']')) {
        return Para::non_prose(t, "editorial_note");
    }
    if t.contains("&amp;") || t.contains("&quot;") || t.contains("&lt;") || t.contains("&gt;") {
        return Para::non_prose(t, "entity_residue");
    }
    if apostrophe_artifact(t) {
        return Para::non_prose(t, "apostrophe_spacing_artifact");
    }
    if lost_character_mark(t, true) {
        return Para::non_prose(t, "lost_character_question_mark");
    }
    Para::classify(t, Screen::XML)
}

/// Inline original-language mark at the start of a speech, `(PL) ...`.
pub fn inline_language(t: &str) -> Option<&str> {
    let b = t.as_bytes();
    (b.len() >= 4 && b[0] == b'(' && b[3] == b')' && b[1].is_ascii_uppercase() && b[2].is_ascii_uppercase()).then(|| &t[1..3])
}

/// Leading report label of a turn's first line (`in writing. - `,
/// `on behalf of the X Group. - `, `rapporteur. - `), if any, as the byte
/// length to strip. Only a label of at most 15 words without a comma or
/// sentence punctuation before `. - ` counts, and it must start in lower
/// case (`in writing`, `rapporteur`) or with an office title.
pub fn role_label(t: &str) -> Option<usize> {
    const OFFICES: &[&str] = &["President-in-Office", "Member of the Commission", "Vice-President of the Commission", "Commissioner", "President of the Commission"];
    let i = t.find(". - ").filter(|&i| i <= 150)?;
    let l = &t[..i];
    let start_ok = l.starts_with(|c: char| c.is_lowercase()) || OFFICES.iter().any(|o| l.starts_with(o));
    (start_ok && text::words(l) <= 15 && !l.contains([',', '?', '!', '.', ';', ':'])).then_some(i + 4)
}

/// Report label and language mark removed from a turn's first line, and
/// the turn's inline original language (if marked).
pub fn turn_lead(first: &str) -> (String, Option<String>, bool) {
    let mut t = first.trim();
    let mut label = false;
    if let Some(n) = role_label(t) {
        t = t[n..].trim_start();
        label = true;
    }
    let lang = inline_language(t).map(str::to_owned);
    if lang.is_some() {
        t = t[4..].trim_start();
    }
    (t.to_owned(), lang, label)
}

/// `ep-YY-MM-DD[-NNN[-NN]].txt` -> ISO date.
pub fn europarl_date(file: &str) -> Option<String> {
    let s = file.strip_prefix("ep-")?;
    let mut p = s.split(['-', '.']);
    let y: u32 = p.next()?.parse().ok()?;
    let m: u32 = p.next()?.parse().ok()?;
    let d: u32 = p.next()?.parse().ok()?;
    iso(if y >= 90 { 1900 + y } else { 2000 + y }, m, d)
}

struct Turn {
    idx: usize,
    name: String,
    language: Option<String>,
    affiliation: Option<String>,
    lines: Vec<String>,
}

fn turns(doc: &str) -> (Vec<Turn>, bool) {
    let mut out: Vec<Turn> = Vec::new();
    let mut marks_language = false;
    let mut cur: Option<Turn> = None;
    for line in doc.lines() {
        let t = line.trim();
        if t.starts_with("<SPEAKER") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            let language = attr(t, "LANGUAGE").map(|l| l.to_uppercase());
            marks_language |= language.is_some();
            cur = Some(Turn { idx: out.len(), name: attr(t, "NAME").unwrap_or_default(), language, affiliation: attr(t, "AFFILIATION"), lines: vec![] });
        } else if t.starts_with("<CHAPTER") {
            if let Some(c) = cur.take() {
                out.push(c);
            }
        } else if t.starts_with('<') {
            // <P> and other markup lines separate paragraphs only.
        } else if !t.is_empty() {
            if let Some(c) = cur.as_mut() {
                if c.lines.is_empty() {
                    marks_language |= inline_language(role_label(t).map_or(t, |n| t[n..].trim_start())).is_some();
                }
                c.lines.push(t.to_owned());
            }
        }
    }
    if let Some(c) = cur.take() {
        out.push(c);
    }
    for (i, t) in out.iter_mut().enumerate() {
        t.idx = i;
    }
    (out, marks_language)
}

pub fn europarl(archive: &Path, archive_url: &str, run: &mut Run) -> Result<Value> {
    let method = format!("europarl_v7_source_release_en_speaker_turn_v1;one_line_one_paragraph;{LOST_CHARACTER_SCREEN};{}", text::PARAGRAPH_RUN_METHOD);
    let f = std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut hr = HashingReader::new(std::io::BufReader::with_capacity(1 << 20, f));
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut files = 0u64;
    let mut files_without_language_marks = 0u64;
    let mut speakers = std::collections::HashSet::new();
    {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(&mut hr));
        for e in ar.entries()? {
            if run.full() {
                break;
            }
            let mut e = e?;
            if !e.header().entry_type().is_file() {
                continue;
            }
            let path = e.path()?.to_string_lossy().into_owned();
            let Some(file) = path.strip_prefix("txt/en/") else { continue };
            if !file.ends_with(".txt") || file.contains('/') {
                continue;
            }
            files += 1;
            let mut b = Vec::new();
            e.read_to_end(&mut b)?;
            let (doc, enc) = decode_text(&b);
            let file_sha = text::sha_hex(&b);
            let Some(date) = europarl_date(file) else {
                run.count("file_without_date");
                continue;
            };
            let (ts, marks) = turns(&doc);
            if !marks {
                files_without_language_marks += 1;
            }
            for t in ts {
                run.units_seen += 1;
                let unit = format!("europarl_turn:{file}:{}", t.idx);
                if chair(&t.name) {
                    run.count("chair_turn");
                    continue;
                }
                let sk = speaker_key(&t.name);
                let wk = contributor_key("europarl_speaker", &sk);
                let (lead, inline, labelled) = t.lines.first().map(|l| turn_lead(l)).unwrap_or_default();
                let original = t.language.as_deref().is_none_or(|l| l == "EN") && inline.as_deref().is_none_or(|l| l == "EN");
                let meta = || json!({"file": file, "turn": t.idx, "date": date, "language_attribute": t.language, "file_marks_language": marks,
                                     "file_sha256": file_sha, "speaker_key": wk});
                if !run.sampled_meta_grouped(&wk, &unit, meta)? {
                    continue;
                }
                let reason = if !marks {
                    Some("file_has_no_original_language_marks")
                } else if !original {
                    Some("translated_from_other_language")
                } else if sk == "|" {
                    Some("speaker_name_unusable")
                } else {
                    None
                };
                if let Some(r) = reason {
                    run.reject(&unit, r)?;
                    continue;
                }
                let mut lines = t.lines.clone();
                if let Some(f) = lines.first_mut() {
                    *f = lead.clone();
                }
                let paras: Vec<Para> = lines.iter().filter(|l| !l.trim().is_empty()).map(|l| europarl_para(l)).collect();
                if labelled {
                    bump(&mut breaks, "role_label_removed");
                }
                for p in &paras {
                    if let Some(r) = p.reason {
                        bump(&mut breaks, r);
                    }
                }
                let Some(ex) = text::paragraph_run(&paras) else {
                    run.reject(&unit, "no_paragraph_run_within_bounds")?;
                    continue;
                };
                if let Some(r) = text::english_screen(&ex.text) {
                    run.reject(&unit, r)?;
                    continue;
                }
                let doc = Doc {
                    id: format!("pool-europarl-{}", &text::sha_hex(unit.as_bytes())[..24]),
                    collection: EUROPARL_COLLECTION,
                    domain: "parliamentary_speech",
                    source_group: unit.clone(),
                    writer_key: Some(wk.clone()),
                    evidence: "historical_proxy",
                    excerpt: &ex,
                    extraction_method: &method,
                    unit: json!({"kind": "europarl_speaker_turn", "file": file, "turn": t.idx, "file_sha256": file_sha, "decoding": enc,
                                 "lead_cleaning": {"role_label_removed": labelled, "inline_language_mark": inline},
                                 "span_basis": "turn_line_index_half_open"}),
                    provenance: json!({"file": path, "turn": t.idx, "sitting_date": date, "speaker_language_attribute": t.language, "inline_language_mark": inline,
                                       "affiliation": t.affiliation, "release": "Europarl v7 source release (statmt.org, 2012-05-15)", "source_url": archive_url}),
                    date: Some(date.clone()),
                    date_basis: "sitting_date_from_file_name",
                    cutoff_policy: "dated_before_cutoff",
                    rights: pile_rights(
                        EUROPARL_LICENSE,
                        format!("Source: European Parliament, debates of {date} (Europarl v7, {file}, turn {})", t.idx),
                        "https://www.europarl.europa.eu/legal-notice/en",
                        "pile-europarl-v1",
                        "European Parliament legal notice authorises reuse, including commercial dissemination, with source acknowledgement; statmt.org states no known copyright restrictions. Only speeches whose original language is English: verbatim-report text as revised by the Parliament.",
                    ),
                    aliases: vec![],
                };
                if run.accept(&unit, &doc)? {
                    *by_year.entry(date[..4].to_owned()).or_default() += 1;
                    speakers.insert(wk);
                }
            }
        }
    }
    std::io::copy(&mut hr, &mut std::io::sink())?;
    let (sha, bytes) = hr.finish();
    Ok(json!({"archive": {"path": archive, "url": archive_url, "sha256": sha, "bytes": bytes}, "english_files": files,
              "english_files_without_language_marks": files_without_language_marks, "paragraph_breaks": breaks, "candidates_by_year": by_year,
              "speakers_with_candidates": speakers.len(),
              "grouping": "unit and source family: speaker turn; bucket, split group and writer_key: folded speaker name (opaque)",
              "limits": ["speaker names change format across years; the order-free key may still split or merge people; 1990s files give surnames only",
                         "a turn without a LANGUAGE attribute or inline (XX) mark is taken as English original only in files that mark other languages by either means",
                         "the report's leading role label (`in writing. -`, `on behalf of the X Group. -`) and an (EN) mark are removed as headers",
                         "verbatim reports are edited by the Parliament; speech is often read from prepared text",
                         "chair turns (President) are skipped"]}))
}

// ---------------------------------------------------------------- NIH ExPORTER

struct Project {
    core: String,
    pi: Option<String>,
    activity: String,
    ic: String,
    date: Option<String>,
    date_basis: &'static str,
    title_sha: String,
    fy: String,
    subproject: String,
}

/// `2008-09-23`, `2008-09-23T00:00:00` or `9/23/2008` -> ISO date.
pub fn nih_date(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 10 && record::valid_date(&s[..10]) {
        return Some(s[..10].to_owned());
    }
    let mut p = s.split(['/', ' ']);
    let m: u32 = p.next()?.parse().ok()?;
    let d: u32 = p.next()?.parse().ok()?;
    let y: u32 = p.next()?.parse().ok()?;
    iso(y, m, d)
}

/// Contact PI id from `PI_IDS` (`123 (contact); 456;`), else the first id.
pub fn contact_pi(ids: &str) -> Option<String> {
    let parts: Vec<&str> = ids.split(';').map(str::trim).filter(|x| !x.is_empty()).collect();
    let pick = parts.iter().find(|x| x.contains("(contact)")).or(parts.first())?;
    let id = pick.replace("(contact)", "").trim().to_owned();
    (!id.is_empty()).then_some(id)
}

const NIH_TAIL_LABELS: &[&str] = &["public health relevance", "project narrative", "relevance to public health", "narrative:", "relevance:"];

/// Applicant description: leading `[unreadable]` and label prefixes
/// (`DESCRIPTION (provided by applicant):`, `PROJECT SUMMARY/ABSTRACT`, ...)
/// removed, and the trailing relevance or narrative section cut.
pub fn nih_description(abs: &str) -> (String, Vec<&'static str>) {
    let mut notes = Vec::new();
    let mut s = abs.trim().to_owned();
    loop {
        // ASCII lowercasing keeps byte offsets aligned with `s`.
        let lower = s.to_ascii_lowercase();
        let before = s.len();
        if lower.starts_with("[unreadable]") {
            s = s["[unreadable]".len()..].trim_start().to_owned();
            notes.push("leading_unreadable_marker_removed");
        } else if let Some(colon) = s.find(':').filter(|&c| c <= 60) {
            let label = lower[..colon].trim();
            let known = ["description", "project summary", "abstract", "summary", "project description", "project abstract"];
            if known.iter().any(|k| label.starts_with(k)) && label.split_whitespace().count() <= 6 {
                s = s[colon + 1..].trim_start().to_owned();
                notes.push("label_prefix_removed");
            }
        } else {
            for k in ["project summary/abstract", "project summary / abstract", "project summary", "abstract"] {
                if lower.starts_with(k) && lower[k.len()..].starts_with(|c: char| c.is_whitespace()) {
                    let rest = &s[k.len()..];
                    // Only an all-caps or title label followed by a capitalised sentence.
                    if rest.trim_start().starts_with(|c: char| c.is_uppercase()) {
                        s = rest.trim_start().to_owned();
                        notes.push("label_prefix_removed");
                    }
                    break;
                }
            }
        }
        if s.len() == before {
            break;
        }
    }
    let lower = s.to_ascii_lowercase();
    if let Some(i) = NIH_TAIL_LABELS.iter().filter_map(|k| lower.find(k)).filter(|&i| i > 0).min() {
        s = s[..i].trim_end().to_owned();
        notes.push("relevance_or_narrative_section_cut");
    }
    (s, notes)
}

/// Paragraphs are blank-line separated blocks; single newlines are hard
/// wraps inside a paragraph (later fiscal years) and are kept as written.
pub fn nih_paragraphs(desc: &str) -> Vec<Para> {
    let norm = desc.replace("\r\n", "\n");
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for l in norm.lines().map(str::trim) {
        if l.is_empty() {
            if !cur.is_empty() {
                blocks.push(cur.join("\n"));
                cur.clear();
            }
        } else {
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        blocks.push(cur.join("\n"));
    }
    blocks
        .into_iter()
        .map(|b| {
            if b.contains("[unreadable]") {
                Para::non_prose(b, "unreadable_marker")
            } else if lost_character_mark(&b, true) {
                Para::non_prose(b, "lost_character_question_mark")
            } else if b.contains('_') || math_notation(&b) {
                Para::non_prose(b, "symbol_or_conversion_residue")
            } else if b.matches('(').count() > b.matches(')').count() || b.matches('[').count() != b.matches(']').count() {
                // `1)` enumerations add closing parentheses; an unclosed
                // opening one or unbalanced square brackets is conversion loss.
                Para::non_prose(b, "unbalanced_brackets")
            } else {
                Para::classify(b, Screen::XML)
            }
        })
        .collect()
}

fn zip_csv(path: &Path) -> Result<(String, &'static str, String, String)> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?)?;
    let name = (0..z.len()).map(|i| z.by_index(i).map(|f| f.name().to_owned())).collect::<Result<Vec<_>, _>>()?.into_iter().find(|n| n.to_lowercase().ends_with(".csv")).context("no csv member")?;
    let mut b = Vec::new();
    z.by_name(&name)?.read_to_end(&mut b)?;
    let sha = text::sha_hex(&b);
    let (s, enc) = decode_text(&b);
    Ok((s, enc, name, sha))
}

fn fy_of(path: &Path) -> Option<String> {
    let n = path.file_name()?.to_str()?;
    let i = n.find("_FY")?;
    Some(n[i + 3..i + 7].to_owned())
}

pub fn nih(projects: &[std::path::PathBuf], abstracts: &[std::path::PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("nih_exporter_applicant_description_v1;{LOST_CHARACTER_SCREEN};{}", text::PARAGRAPH_RUN_METHOD);
    let mut proj_by_fy: HashMap<String, std::path::PathBuf> = HashMap::new();
    for p in projects {
        proj_by_fy.insert(fy_of(p).context("project file without FY")?, p.clone());
    }
    let mut files = Vec::new();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut notes_n: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_fy: BTreeMap<String, u64> = BTreeMap::new();
    let mut pis = std::collections::HashSet::new();
    for a in abstracts {
        let fy = fy_of(a).context("abstract file without FY")?;
        let pp = proj_by_fy.get(&fy).with_context(|| format!("no project file for FY{fy}"))?;
        let (ptext, penc, pname, psha) = zip_csv(pp)?;
        let mut t = crate::csvread::Table::new(&ptext)?;
        let col = |t: &crate::csvread::Table, n: &str| t.col(n).with_context(|| format!("FY{fy} projects: no column {n}"));
        let (c_app, c_core, c_pi, c_act, c_ic, c_award, c_bstart, c_pstart, c_title, c_fy) =
            (col(&t, "APPLICATION_ID")?, col(&t, "CORE_PROJECT_NUM")?, col(&t, "PI_IDS")?, col(&t, "ACTIVITY")?, col(&t, "ADMINISTERING_IC")?,
             col(&t, "AWARD_NOTICE_DATE")?, col(&t, "BUDGET_START")?, col(&t, "PROJECT_START")?, col(&t, "PROJECT_TITLE")?, col(&t, "FY")?);
        let c_sub = t.col("SUBPROJECT_ID");
        let mut proj: HashMap<String, Project> = HashMap::new();
        while let Some(r) = t.next_row()? {
            let (date, basis) = match (nih_date(&r[c_award]), nih_date(&r[c_bstart]), nih_date(&r[c_pstart])) {
                (Some(d), _, _) => (Some(d), "award_notice_date"),
                (None, Some(d), _) => (Some(d), "budget_start"),
                (None, None, Some(d)) => (Some(d), "project_start"),
                _ => (None, "none"),
            };
            proj.insert(r[c_app].trim().to_owned(), Project {
                core: r[c_core].trim().to_owned(), pi: contact_pi(&r[c_pi]), activity: r[c_act].trim().to_owned(), ic: r[c_ic].trim().to_owned(),
                date, date_basis: basis, title_sha: text::sha_hex(r[c_title].as_bytes()), fy: r[c_fy].trim().to_owned(),
                subproject: c_sub.map(|c| r[c].trim().to_owned()).unwrap_or_default(),
            });
        }
        let (atext, aenc, aname, asha) = zip_csv(a)?;
        let mut t = crate::csvread::Table::new(&atext)?;
        let (c_app, c_abs) = (t.col("APPLICATION_ID").context("APPLICATION_ID")?, t.col("ABSTRACT_TEXT").context("ABSTRACT_TEXT")?);
        let mut rows = 0u64;
        while let Some(r) = t.next_row()? {
            if run.full() {
                break;
            }
            rows += 1;
            run.units_seen += 1;
            let app = r[c_app].trim().to_owned();
            let unit = format!("nih_application:{app}");
            let Some(p) = proj.get(&app) else {
                run.count("abstract_without_project_row");
                continue;
            };
            let Some(pi) = p.pi.as_deref() else {
                run.count("project_without_pi");
                continue;
            };
            let pk = contributor_key("nih_contact_pi", pi);
            let family = if p.core.is_empty() { unit.clone() } else { format!("nih_core_project:{}", p.core) };
            let meta = || json!({"application_id": app, "core_project": p.core, "fy": p.fy, "activity": p.activity, "date": p.date,
                                 "abstract_sha256": text::sha_hex(r[c_abs].as_bytes()), "pi_key": pk});
            if !run.sampled_meta_grouped(&pk, &family, meta)? {
                continue;
            }
            let Some(date) = p.date.clone().filter(|d| record::before_cutoff(d)) else {
                run.reject(&unit, "no_date_before_cutoff")?;
                continue;
            };
            if fy.as_str() > "2022" {
                run.reject(&unit, "fiscal_year_after_2022")?;
                continue;
            }
            let (desc, notes) = nih_description(&r[c_abs]);
            for n in &notes {
                bump(&mut notes_n, n);
            }
            let paras = nih_paragraphs(&desc);
            for x in &paras {
                if let Some(rr) = x.reason {
                    bump(&mut breaks, rr);
                }
            }
            let Some(ex) = text::paragraph_run(&paras) else {
                run.reject(&unit, "no_paragraph_run_within_bounds")?;
                continue;
            };
            if let Some(rr) = text::english_screen(&ex.text) {
                run.reject(&unit, rr)?;
                continue;
            }
            let doc = Doc {
                id: format!("pool-nih-{app}"),
                collection: NIH_COLLECTION,
                domain: "grant_abstract_biomedical",
                source_group: family.clone(),
                writer_key: Some(pk.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "nih_exporter_abstract_description", "application_id": app, "abstract_file": aname, "abstract_csv_sha256": asha,
                             "abstract_text_sha256": text::sha_hex(r[c_abs].as_bytes()), "cleaning": notes, "span_basis": "description_paragraph_index_half_open"}),
                provenance: json!({"application_id": app, "core_project_num": p.core, "subproject_id": p.subproject, "fy": p.fy, "activity": p.activity,
                                   "administering_ic": p.ic, "project_title_sha256": p.title_sha, "source_url": format!("https://reporter.nih.gov/project-details/{app}"),
                                   "release": "NIH RePORTER ExPORTER fiscal-year CSVs", "download": format!("https://reporter.nih.gov/exporter/abstracts/download/{fy}")}),
                date: Some(date.clone()),
                date_basis: p.date_basis,
                cutoff_policy: "dated_before_cutoff",
                rights: pile_rights(
                    NIH_LICENSE,
                    format!("NIH RePORTER project abstract, application {app} (core project {}), FY{}", p.core, p.fy),
                    "https://report.nih.gov/faqs",
                    "pile-nih-exporter-v1",
                    "Bulk ExPORTER files are offered publicly by NIH for individual use and analyses; no licence or non-commercial restriction is stated. Abstracts are written by the applicants. PI ids are opaque grouping keys; no names are stored.",
                ),
                aliases: vec![],
            };
            if run.accept(&unit, &doc)? {
                *by_fy.entry(fy.clone()).or_default() += 1;
                pis.insert(pk);
            }
        }
        files.push(json!({"fy": fy, "abstracts": {"zip": a, "member": aname, "csv_sha256": asha, "decoding": aenc, "rows": rows},
                          "projects": {"zip": pp, "member": pname, "csv_sha256": psha, "decoding": penc, "rows": proj.len()}}));
    }
    Ok(json!({"files": files, "paragraph_breaks": breaks, "cleaning_counts": notes_n, "candidates_by_fy": by_fy, "pis_with_candidates": pis.len(),
              "tail_labels": NIH_TAIL_LABELS,
              "grouping": "source family: core project number; bucket, split group and writer_key: contact PI id (opaque)",
              "limits": ["the abstract may be drafted by any member of the applicant team; the contact PI is a grouping key, not an authorship claim",
                         "abstracts recur across fiscal years of one project; exact and near duplicates are removed at admission",
                         "one long single-paragraph abstract over 450 words yields nothing (no truncation)"]}))
}

/// Characters that mark formula or symbol residue in flattened text.
pub fn math_notation(t: &str) -> bool {
    t.chars().any(|c| matches!(c as u32, 0x2061..=0x2064 | 0x2200..=0x22FF | 0x2190..=0x21FF | 0x25A0..=0x25FF))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Excerpt;

    #[test]
    fn enron_own_part_cuts_quoted_and_lotus_blocks() {
        let body = "Karen,\n\nPlease send the files today. We need them before the meeting.\n\nThanks,\nPat\n\n -----Original Message-----\nFrom: someone\nSent: today\n\nold text";
        let (own, why) = own_part(body);
        assert_eq!(why, Some("original_or_forwarded_message"));
        assert!(own.ends_with("Thanks,\nPat\n"));
        let lotus = "We agree with the plan and will proceed next week as discussed.\n\nJohn Doe\n10/16/2000 01:42 PM\nTo: Jane Roe/HOU/ECT\ncc:\nSubject: plan\n\nquoted";
        let (own, why) = own_part(lotus);
        assert_eq!(why, Some("embedded_header"));
        assert_eq!(own.trim(), "We agree with the plan and will proceed next week as discussed.");
    }

    #[test]
    fn enron_headers_dates_and_owner() {
        let (h, body) = split_message("Message-ID: <1.2@x>\r\nDate: Thu, 7 Jun 2001 06:51:25 -0700 (PDT)\r\nFrom: jo.sample@example.com\r\nSubject: a\r\n  continued\r\n\r\nbody");
        assert_eq!(header(&h, "subject"), "a continued");
        assert_eq!(body, "body");
        assert_eq!(rfc822_date(header(&h, "date")).as_deref(), Some("2001-06-07"));
        assert!(from_owner(header(&h, "from"), "sample-j"));
        assert!(!from_owner("assistant.name@example.com", "sample-j"));
    }

    #[test]
    fn email_paragraph_screen_breaks_contact_and_residue() {
        assert_eq!(email_para("Call me at 555-010-0199 tomorrow.".into()).reason, Some("contact_details"));
        assert_eq!(email_para("Send it to bob@example.com please.".into()).reason, Some("address_or_header_residue"));
        assert_eq!(email_para("This is fine=20 text".into()).reason, Some("quoted_printable_residue"));
        assert!(email_para("Thanks,\nPat".into()).prose);
    }

    #[test]
    fn europarl_turns_language_and_keys() {
        let doc = "<CHAPTER ID=1>\nTitle\n<SPEAKER ID=1 NAME=\"President\">\nOpening words of the sitting.\n<SPEAKER ID=2 LANGUAGE=\"FR\" NAME=\"Dupont, Jean\">\nTranslated text here.\n<P>\nMore.\n<SPEAKER ID=\"3\" NAME=\"Robert Evans\" AFFILIATION=\"PSE\">\nEnglish text.\n";
        let (t, marks) = turns(doc);
        assert!(marks);
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].language.as_deref(), Some("FR"));
        assert_eq!(t[1].lines.len(), 2);
        assert_eq!(t[2].affiliation.as_deref(), Some("PSE"));
        assert!(chair(&t[0].name));
        assert!(!chair("Evans, Robert J"));
        assert_eq!(speaker_key("Evans, Robert J"), speaker_key("Robert Evans"));
        assert_eq!(speaker_key("Fischer Boel, Mariann"), speaker_key("Mariann Fischer Boel (Commission)"));
        assert_eq!(europarl_date("ep-00-01-17.txt").as_deref(), Some("2000-01-17"));
        assert_eq!(europarl_date("ep-96-04-15-002.txt").as_deref(), Some("1996-04-15"));
        assert!(apostrophe_artifact("a minute' s silence"));
        assert!(!apostrophe_artifact("the members' votes"));
        assert_eq!(europarl_para("(The House rose)").reason, Some("editorial_note"));
        assert_eq!(turn_lead("in writing. - (IT) Madam President, I agree."), ("Madam President, I agree.".into(), Some("IT".into()), true));
        assert_eq!(turn_lead("on behalf of the PSE Group. - Madam President, we agree."), ("Madam President, we agree.".into(), None, true));
        assert_eq!(turn_lead("(EN) Thank you."), ("Thank you.".into(), Some("EN".into()), false));
        assert_eq!(turn_lead("Mr President, I agree. - with reservations."), ("Mr President, I agree. - with reservations.".into(), None, false));
        assert_eq!(turn_lead("President-in-Office of the Council. - (FR) Thank you."), ("Thank you.".into(), Some("FR".into()), true));
        assert_eq!(speaker_key("Bowis"), "bowis|");
        let (_, marks) = turns("<SPEAKER ID=1 NAME=\"A B\">\n(PL) Madam President.\n");
        assert!(marks);
        assert!(rewrapped("one two three four five six seven eight nine\nten\neleven twelve thirteen fourteen fifteen sixteen seventeen eighteen\nnineteen\nend of it"));
        assert!(!rewrapped("Thanks,\nPat"));
    }

    #[test]
    fn nih_cleaning_and_pi() {
        let (d, n) = nih_description("  [unreadable] DESCRIPTION (provided by applicant): The goal is to study cells. PUBLIC HEALTH RELEVANCE: This matters.");
        assert_eq!(d, "The goal is to study cells.");
        assert_eq!(n, vec!["leading_unreadable_marker_removed", "label_prefix_removed", "relevance_or_narrative_section_cut"]);
        let (d, _) = nih_description("PROJECT SUMMARY/ABSTRACT The aim of this work is clear.");
        assert_eq!(d, "The aim of this work is clear.");
        let (d, n) = nih_description("Cells divide: a study of mitosis in yeast.");
        assert_eq!(d, "Cells divide: a study of mitosis in yeast.");
        assert!(n.is_empty());
        assert_eq!(contact_pi("123 (contact); 456;").as_deref(), Some("123"));
        assert_eq!(contact_pi("789;").as_deref(), Some("789"));
        assert_eq!(nih_date("9/23/2008").as_deref(), Some("2008-09-23"));
        assert_eq!(nih_date("2008-09-23T00:00:00").as_deref(), Some("2008-09-23"));
        assert_eq!(nih_paragraphs("Text with [unreadable] mark inside it here now.")[0].reason, Some("unreadable_marker"));
        assert_eq!(nih_paragraphs("The aims are: 1) to test cells and 2) to grow them in culture.")[0].reason, None);
        let p = nih_paragraphs("The aims are to test\ncells in culture.\n\nSecond block of text ends here.");
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].text, "The aims are to test\ncells in culture.");
        let (d, _) = nih_description("ABSTRACT\nCancer is common in many groups of people.");
        assert_eq!(d, "Cancer is common in many groups of people.");
        assert_eq!(nih_paragraphs("The receptor ([52AR) binds the ligand in all of the cells.")[0].reason, Some("unbalanced_brackets"));
    }

    #[test]
    fn symbols_and_licences() {
        assert!(math_notation("x \u{2208} A"));
        assert!(!math_notation("about 5 °C ± 2"));
        assert!(record::admitted_license(ENRON_LICENSE) && record::admitted_license(NIH_LICENSE) && record::admitted_license(EUROPARL_LICENSE));
    }

    #[test]
    fn lost_character_marks() {
        for t in ["the Children?s Hospital", "kept at -80?C overnight", "a ?jumping gene? here", "the ECM ? a matrix", "e.g.??fetal", "??Utility executives"] {
            assert!(lost_character_mark(t, false), "{t}");
        }
        for t in ["ER? binds the ligand", "the 'ambiguous? and the", "called TGF?, which"] {
            assert!(lost_character_mark(t, true) && !lost_character_mark(t, false), "{t}");
        }
        for t in ["Is it ready?  We'll see.", "Can we argue this???  DF", "How? This proposal works.", "Why not?\nBecause.", "ok?"] {
            assert!(!lost_character_mark(t, true), "{t}");
        }
        assert_eq!(nih_paragraphs("Alzheimer?s disease affects many people in the world today.")[0].reason, Some("lost_character_question_mark"));
        assert_eq!(europarl_para("Mr President, the Dalai Lama?s proposal deserves support from all of us.").reason, Some("lost_character_question_mark"));
        assert_eq!(email_para("??Utility executives said the plan would work for everyone involved here.".into()).reason, Some("lost_character_question_mark"));
    }

    #[test]
    fn windows_1252_fallback() {
        let (s, enc) = decode_text(b"caf\xe9 \x93q\x94\nna\xc3\xafve");
        assert_eq!(enc, "utf-8-with-windows-1252-lines");
        assert_eq!(s, "café “q”\nnaïve");
    }

    #[test]
    fn contributor_keys_are_opaque() {
        let k = contributor_key("enron_mailbox", "sample-j");
        assert!(k.starts_with("enron_mailbox:") && !k.contains("sample"));
    }

    #[test]
    fn email_paragraph_run_end_to_end() {
        let sentence = "We should review the contract terms with the legal team before we sign anything this week. ";
        let body = format!("John,\n\n{}\n\n{}\n\nThanks,\nPat\n\n-----Original Message-----\nold", sentence.repeat(9), sentence.repeat(9));
        let (own, _) = own_part(&body);
        let paras = email_paragraphs(&own);
        let ex: Excerpt = text::paragraph_run(&paras).expect("run");
        assert!(ex.text.starts_with("John,"));
        assert!(text::words(&ex.text) >= 150);
    }
}

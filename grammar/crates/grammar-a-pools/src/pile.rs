//! Human components of the Pile, read from their original releases so the
//! writer metadata the Pile dropped is available for grouping:
//!
//! * **Enron emails** (CMU 2015-05-07 release of the FERC public record):
//!   only messages in the mailbox owner's own sent folders whose `From`
//!   address names the owner; quoted, forwarded and header material is cut
//!   before paragraphs are taken. Bucket, split group and `writer_key`: the
//!   mailbox owner (opaque key); source family: the message.
//!   - **Reserved mailboxes** (buckets 900..1000; B22): `enron_reserved`
//!     renders them at Stage 1 under the same admission rules as an ungated
//!     diagnostic, never trained on and never hosted, with the prior set of
//!     protocol A2 amendment 23; `enron_ood_restrict` restricts R0's
//!     out-of-domain email selection to S0-9's diagnostic-half mailboxes.
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
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
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

const ENRON_NOTES: &str = "Made public by the US Federal Energy Regulatory Commission during its investigation; CMU distributes the corpus for research and states no licence or use restriction. Personal-data hygiene: owner ids are opaque; text stays in ignored storage.";

/// The population an Enron run renders.
pub enum EnronMode<'a> {
    /// The enron-v1 admission's adapter (cohort `pile-enron-v1`). A message
    /// of a reserved mailbox (buckets 900..1000) inside the run's range
    /// stops the run unless it is a metadata-only listing.
    Train,
    /// The mailboxes held out of that admission (B22): the same rules on the
    /// listed messages only, rows marked diagnostic only and never hosted.
    ReservedDiagnostic(&'a EnronReservation),
    /// Tests only: the Train rules and rights without the reserved-bucket
    /// stop, to compare the reserved render with the Train adapter.
    #[cfg(test)]
    TrainRulesForComparison,
}

/// The Train adapter. It never renders a reserved mailbox's prose: see
/// `EnronMode::Train`.
pub fn enron(archive: &Path, archive_url: &str, run: &mut Run) -> Result<Value> {
    enron_with(archive, archive_url, run, &EnronMode::Train)
}

/// The Enron adapter body. In `ReservedDiagnostic` mode the run must be
/// restricted to `ENRON_RESERVED_BUCKETS` and to the listing's messages
/// (`enron_reserved_render` sets both): every message of an in-range mailbox
/// must be listed, every listed message must be found, and every rendered
/// row's writer is a listed reserved mailbox.
pub fn enron_with(archive: &Path, archive_url: &str, run: &mut Run, mode: &EnronMode) -> Result<Value> {
    let method = format!("enron_sent_folder_own_part_v1;email_paragraph_screen_v1;{LOST_CHARACTER_SCREEN};{}", text::PARAGRAPH_RUN_METHOD);
    let f = std::fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut hr = HashingReader::new(std::io::BufReader::with_capacity(1 << 20, f));
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut cuts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut owners_with: BTreeMap<String, u64> = BTreeMap::new();
    let mut messages = 0u64;
    let mut listed_seen: HashSet<String> = HashSet::new();
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
            if matches!(mode, EnronMode::Train) && !run.is_metadata_only() && run.in_range(&owner_key) && crate::adapter::bucket(&owner_key) >= ENRON_RESERVED_BUCKETS.0 {
                bail!("the enron adapter renders reserved mailbox buckets {}..{} only as a metadata-only listing; their prose is rendered by enron-reserved", ENRON_RESERVED_BUCKETS.0, ENRON_RESERVED_BUCKETS.1);
            }
            let mut b = Vec::new();
            e.read_to_end(&mut b)?;
            let (raw, enc) = decode_text(&b);
            let (h, body) = split_message(&raw);
            let mid = header(&h, "message-id").to_owned();
            let unit = format!("enron_message:{}", &text::sha_hex(format!("{owner}\0{mid}\0{path}").as_bytes())[..24]);
            let date = rfc822_date(header(&h, "date"));
            if let EnronMode::ReservedDiagnostic(res) = mode
                && run.in_range(&owner_key)
            {
                ensure!(res.owner_rows.contains_key(&owner_key), "an in-range mailbox is not a listed reserved mailbox");
                ensure!(res.units.contains(&unit), "a sent message of a reserved mailbox is not in the reservation listing: the archive differs from the reservation scan");
                ensure!(listed_seen.insert(unit.clone()), "a reserved message occurs twice in the archive");
            }
            let meta = || json!({"archive_path": path, "message_id_sha256": text::sha_hex(mid.as_bytes()), "date": date, "folder": parts[2],
                                 "raw_sha256": text::sha_hex(&b), "owner_key": owner_key});
            if !run.sampled_meta_grouped(&owner_key, &unit, meta)? {
                continue;
            }
            if let EnronMode::ReservedDiagnostic(res) = mode {
                ensure!(res.owner_rows.contains_key(&owner_key) && listed_seen.contains(&unit), "a message passed the gate outside the reservation listing");
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
                rights: {
                    let attribution = format!("Enron Email Dataset (CMU, May 7, 2015 version), message {path}; sender is the mailbox owner (opaque writer_key)");
                    match mode {
                        EnronMode::Train => pile_rights(ENRON_LICENSE, attribution, "https://www.cs.cmu.edu/~enron/", "pile-enron-v1", ENRON_NOTES),
                        #[cfg(test)]
                        EnronMode::TrainRulesForComparison => pile_rights(ENRON_LICENSE, attribution, "https://www.cs.cmu.edu/~enron/", "pile-enron-v1", ENRON_NOTES),
                        EnronMode::ReservedDiagnostic(_) => {
                            diagnostic_rights(pile_rights(ENRON_LICENSE, attribution, "https://www.cs.cmu.edu/~enron/", ENRON_DIAGNOSTIC_COHORT, ENRON_NOTES))
                        }
                    }
                },
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
    let mut extra = json!({"archive": {"path": archive, "url": archive_url, "sha256": sha, "bytes": bytes}, "messages_in_archive": messages,
              "sent_folders": ENRON_SENT_FOLDERS, "own_part_cut_markers": cuts, "paragraph_breaks": breaks, "candidates_by_year": by_year,
              "owners_with_candidates": owners_with.len(),
              "grouping": "unit and source family: message; bucket, split group and writer_key: mailbox owner (opaque)",
              "limits": ["only the owner's sent folders; the From address must contain the owner's surname, so messages sent by assistants under another address are excluded",
                         "a message an assistant typed from the owner's account cannot be separated",
                         "unmarked forwarded or pasted text inside the owner's part is not detected",
                         "greetings and sign-offs are kept as written; addresses, phone numbers and quoted-printable residue break runs"]});
    if let EnronMode::ReservedDiagnostic(res) = mode {
        ensure!(listed_seen.len() == res.units.len(), "{} of {} listed reserved messages were not found in the archive", res.units.len() - listed_seen.len(), res.units.len());
        extra["mode"] = json!("reserved_diagnostic");
        extra["reservation"] = json!({"bucket_range": [ENRON_RESERVED_BUCKETS.0, ENRON_RESERVED_BUCKETS.1], "listed_messages": res.units.len(),
                                      "listed_mailboxes": res.owner_rows.len(), "listed_messages_found": listed_seen.len(),
                                      "rule": "every sent message of an in-range mailbox is listed and every listed message is found, so the rendered set is exactly the reservation scan's"});
    }
    Ok(extra)
}

// ------------------------------------------- Enron reserved mailboxes (B22)

/// Mailbox buckets held out of the enron-v1 admission, listed there as
/// text-free metadata only (`reserved-metadata.jsonl`).
pub const ENRON_RESERVED_BUCKETS: (u32, u32) = (900, 1000);
/// `VALUES-R2.json` key of the Enron halves (protocol A2 V23, A9).
pub const ENRON_HALVES_KEY: &str = "S0-9";
/// V23: writer keys ordered by SHA-256(prefix + key), ascending.
pub const ENRON_HALF_PLAN_PREFIX: &str = "slop_ninja_a_comp_r2_plan_v1:enron_half:";
/// V23: the first 50 keys in plan-key order form the Train half.
pub const ENRON_TRAIN_HALF_KEYS: usize = 50;
/// Cohort of the reserved-mailbox diagnostic rows.
pub const ENRON_DIAGNOSTIC_COHORT: &str = "pile-enron-reserved-diagnostic-v1";
pub const ENRON_RESERVED_SCHEMA: &str = "slop_ninja_a_enron_reserved_diagnostic_v1";
pub const OOD_EMAIL_SCHEMA: &str = "slop_ninja_a_ood_email_diagnostic_half_v1";
/// Schema and pool name of the R0 out-of-domain human selection.
pub const OOD_SELECTION_SCHEMA: &str = "slop_ninja_a_ood_human_diag_selection_v1";
pub const OOD_ENRON_POOL: &str = "enron-v1";

/// A Train-side Enron render (`enron`) may not render the reserved buckets:
/// a range reaching into 900..1000 (or no range) is allowed only as a
/// metadata-only listing. Their prose is rendered only by `enron_reserved`.
pub fn enron_train_buckets(buckets: Option<(u32, u32)>, metadata_only: bool) -> Result<()> {
    let reaches = buckets.is_none_or(|(_, hi)| hi > ENRON_RESERVED_BUCKETS.0);
    ensure!(metadata_only || !reaches, "the enron adapter renders reserved mailbox buckets {}..{} only as a metadata-only listing; their prose is rendered by enron-reserved", ENRON_RESERVED_BUCKETS.0, ENRON_RESERVED_BUCKETS.1);
    Ok(())
}

/// Rights of a reserved-mailbox row: a human FPR diagnostic only, never
/// trained on, never sent to a hosted model (admission caveat; A2 §2.7).
pub fn diagnostic_rights(mut r: Value) -> Value {
    r["evaluation_use"] = json!("diagnostic_only");
    r["train_use"] = json!(false);
    r["hosted_model_use"] = json!("never");
    r["diagnostic"] = json!({"population": "enron_reserved_mailboxes", "buckets": [ENRON_RESERVED_BUCKETS.0, ENRON_RESERVED_BUCKETS.1],
                             "use": "ungated human false-positive-rate diagnostic (protocol A2 section 5, T3d)"});
    r
}

/// `enron_mailbox:` and 20 lowercase hex characters (`contributor_key`).
fn mailbox_key_ok(k: &str) -> bool {
    k.strip_prefix("enron_mailbox:").is_some_and(|h| h.len() == 20 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
}

/// SHA-256 of the keys joined by newline with a trailing newline (S0-9's
/// `list_sha256_rule`).
pub fn key_list_sha256(keys: &[String]) -> String {
    let mut s = keys.join("\n");
    s.push('\n');
    text::sha_hex(s.as_bytes())
}

pub fn enron_half_plan_key(k: &str) -> String {
    text::sha_hex(format!("{ENRON_HALF_PLAN_PREFIX}{k}").as_bytes())
}

/// The Enron halves of S0-9, verified: list digests and counts, V23's
/// plan-key order with the first 50 keys as the Train half, the halves'
/// mailboxes in rendered buckets (below 900) and the reserved ones in
/// 900..1000, sorted, and the three lists disjoint.
#[derive(Debug, Clone)]
pub struct EnronHalves {
    pub train: Vec<String>,
    pub diagnostic: Vec<String>,
    pub reserved: Vec<String>,
    /// S0-9's bound inputs: file name -> (recorded path, sha256, bytes).
    pub inputs: BTreeMap<String, (String, String, u64)>,
}

impl EnronHalves {
    pub fn from_s09(v: &Value) -> Result<Self> {
        ensure!(v["id"] == ENRON_HALVES_KEY && v["name"] == "enron_halves", "not the S0-9 Enron halves record");
        ensure!(v["plan_key_prefix"] == ENRON_HALF_PLAN_PREFIX, "S0-9 plan-key prefix differs from V23's");
        let list = |sec: &str, field: &str, count: &str| -> Result<Vec<String>> {
            let s = &v[sec];
            let keys: Vec<String> = s[field].as_array().with_context(|| format!("S0-9 {sec}.{field}"))?.iter()
                .map(|x| x.as_str().map(str::to_owned).with_context(|| format!("S0-9 {sec}: non-string key")))
                .collect::<Result<_>>()?;
            ensure!(s[count].as_u64() == Some(keys.len() as u64), "S0-9 {sec}: {count} differs from the list length {}", keys.len());
            ensure!(s["list_sha256"] == key_list_sha256(&keys).as_str(), "S0-9 {sec}: list_sha256 does not match its keys");
            ensure!(keys.iter().all(|k| mailbox_key_ok(k)), "S0-9 {sec}: a key is not an opaque mailbox key");
            Ok(keys)
        };
        let train = list("train_half", "writer_keys_in_plan_key_order", "writer_keys")?;
        let diagnostic = list("diagnostic_half", "writer_keys_in_plan_key_order", "writer_keys")?;
        let reserved = list("reserved_mailboxes_diagnostic", "owner_keys_sorted", "owner_keys")?;
        ensure!(train.len() == ENRON_TRAIN_HALF_KEYS, "V23: the Train half is the first {ENRON_TRAIN_HALF_KEYS} keys, S0-9 lists {}", train.len());
        ensure!(!diagnostic.is_empty() && !reserved.is_empty(), "S0-9: an empty half");
        let order: Vec<String> = train.iter().chain(&diagnostic).map(|k| enron_half_plan_key(k)).collect();
        ensure!(order.windows(2).all(|w| w[0] < w[1]), "S0-9: the halves are not in V23's plan-key order");
        ensure!(reserved.windows(2).all(|w| w[0] < w[1]), "S0-9: reserved mailbox keys are not sorted and distinct");
        let (lo, hi) = ENRON_RESERVED_BUCKETS;
        for k in train.iter().chain(&diagnostic) {
            ensure!(crate::adapter::bucket(k) < lo, "S0-9: an admitted writer key hashes into the reserved buckets");
        }
        for k in &reserved {
            let b = crate::adapter::bucket(k);
            ensure!(lo <= b && b < hi, "S0-9: a reserved mailbox key hashes to bucket {b}, outside {lo}..{hi}");
        }
        let c = &v["checks"];
        ensure!(c["writer_keys"].as_u64() == Some((train.len() + diagnostic.len()) as u64), "S0-9 checks.writer_keys differs from the halves");
        ensure!(c["reserved_owner_keys"].as_u64() == Some(reserved.len() as u64), "S0-9 checks.reserved_owner_keys differs from the list");
        let mut inputs = BTreeMap::new();
        for i in v["inputs"].as_array().context("S0-9 inputs")? {
            let p = i["path"].as_str().context("S0-9 input path")?;
            let name = Path::new(p).file_name().and_then(|n| n.to_str()).context("S0-9 input name")?.to_owned();
            let sha = i["sha256"].as_str().context("S0-9 input sha256")?.to_owned();
            ensure!(inputs.insert(name.clone(), (p.to_owned(), sha, i["bytes"].as_u64().unwrap_or(0))).is_none(), "S0-9 binds two inputs named {name}");
        }
        Ok(EnronHalves { train, diagnostic, reserved, inputs })
    }
    /// Read `values.S0-9` from `VALUES-R2.json`; returns the halves and the
    /// file's SHA-256.
    pub fn from_values_file(path: &Path) -> Result<(Self, String)> {
        let b = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let v: Value = serde_json::from_slice(&b)?;
        ensure!(v["schema"] == "slop_ninja_a_round2_values_v1", "{} is not VALUES-R2.json", path.display());
        Ok((Self::from_s09(&v["values"][ENRON_HALVES_KEY]).with_context(|| format!("{} values.{ENRON_HALVES_KEY}", path.display()))?, text::sha_hex(&b)))
    }
    pub fn admitted(&self) -> BTreeSet<&str> {
        self.train.iter().chain(&self.diagnostic).map(String::as_str).collect()
    }
    /// The recorded SHA-256 of S0-9's input named `name`.
    pub fn input_sha(&self, name: &str) -> Result<&str> {
        Ok(self.inputs.get(name).with_context(|| format!("S0-9 binds no input named {name}"))?.1.as_str())
    }
}

/// `sha256sum` output: path (leading `./` removed) -> SHA-256.
pub fn parse_sha256sums(s: &str) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for l in s.lines().filter(|l| !l.trim().is_empty()) {
        let (sha, name) = l.split_once("  ").or_else(|| l.split_once(" *")).with_context(|| format!("SHA256SUMS line: {l}"))?;
        ensure!(sha.len() == 64 && sha.bytes().all(|c| c.is_ascii_hexdigit()), "SHA256SUMS digest: {l}");
        let name = name.strip_prefix("./").unwrap_or(name).to_owned();
        ensure!(m.insert(name, sha.to_owned()).is_none(), "SHA256SUMS lists a file twice: {l}");
    }
    Ok(m)
}

/// An input file checked against an expected SHA-256.
fn bound(path: &Path, expect: &str, what: &str) -> Result<Value> {
    let (sha, bytes) = crate::io::file_sha(path).with_context(|| format!("{what}: {}", path.display()))?;
    ensure!(sha == expect, "{what} {} has sha256 {sha}, expected {expect}", path.display());
    Ok(json!({"role": what, "path": path, "sha256": sha, "bytes": bytes}))
}

/// An admitted pool directory bound by its `SHA256SUMS`.
pub struct BoundPool {
    pub dir: PathBuf,
    pub sums: BTreeMap<String, String>,
    pub sums_sha256: String,
}

impl BoundPool {
    /// `expect_sums`: the SHA-256 the pool's `SHA256SUMS` must have, if known.
    pub fn open(dir: &Path, expect_sums: Option<&str>) -> Result<Self> {
        let p = dir.join("SHA256SUMS");
        let b = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
        let sums_sha256 = text::sha_hex(&b);
        if let Some(e) = expect_sums {
            ensure!(sums_sha256 == e, "{} has sha256 {sums_sha256}, S0-9 binds {e}", p.display());
        }
        Ok(BoundPool { dir: dir.to_owned(), sums: parse_sha256sums(std::str::from_utf8(&b)?)?, sums_sha256 })
    }
    pub fn sha(&self, name: &str) -> Result<&str> {
        Ok(self.sums.get(name).with_context(|| format!("{}/SHA256SUMS lists no {name}", self.dir.display()))?.as_str())
    }
    /// The file, checked against its `SHA256SUMS` line.
    pub fn file(&self, name: &str) -> Result<(PathBuf, Value)> {
        let p = self.dir.join(name);
        let v = bound(&p, self.sha(name)?, name)?;
        Ok((p, v))
    }
    /// The one listed file named `prefix…suffix`.
    pub fn one(&self, prefix: &str, suffix: &str) -> Result<String> {
        let m: Vec<&String> = self.sums.keys().filter(|k| k.starts_with(prefix) && k.ends_with(suffix)).collect();
        ensure!(m.len() == 1, "{}/SHA256SUMS lists {} files {prefix}*{suffix}, expected one", self.dir.display(), m.len());
        Ok(m[0].clone())
    }
}

/// The reservation scan's text-free listing of the reserved mailboxes'
/// messages, checked against S0-9.
#[derive(Debug, Default)]
pub struct EnronReservation {
    /// Source-family (message unit) keys.
    pub units: HashSet<String>,
    /// Listed rows per reserved mailbox key.
    pub owner_rows: BTreeMap<String, u64>,
    pub rows: u64,
}

impl EnronReservation {
    /// Every row: the metadata-only schema, no text, prose never opened,
    /// group and owner key equal, bucket = `bucket(owner)` inside
    /// `ENRON_RESERVED_BUCKETS`, owner a reserved mailbox of S0-9, distinct
    /// message units; every reserved mailbox has rows.
    pub fn from_rows(rows: impl IntoIterator<Item = Result<Value>>, halves: &EnronHalves) -> Result<Self> {
        let reserved: BTreeSet<&str> = halves.reserved.iter().map(String::as_str).collect();
        let (lo, hi) = ENRON_RESERVED_BUCKETS;
        let mut r = EnronReservation::default();
        for row in rows {
            let row = row?;
            let n = r.rows + 1;
            ensure!(row["schema"] == crate::adapter::RESERVED_METADATA_SCHEMA, "reservation row {n}: not a metadata-only row");
            ensure!(row.get("text").is_none() && row["prose_opened"] == false, "reservation row {n}: carries or opened prose");
            let owner = row["owner_key"].as_str().with_context(|| format!("reservation row {n}: owner_key"))?;
            ensure!(row["group_key"] == owner && row["bucket_basis"] == "group_key", "reservation row {n}: bucket key is not the mailbox owner");
            let b = crate::adapter::bucket(owner);
            ensure!(row["bucket"].as_u64() == Some(b as u64) && lo <= b && b < hi, "reservation row {n}: bucket is not bucket(owner) in {lo}..{hi}");
            ensure!(reserved.contains(owner), "reservation row {n}: the mailbox is not an S0-9 reserved mailbox");
            let unit = row["family_key"].as_str().with_context(|| format!("reservation row {n}: family_key"))?;
            ensure!(unit.starts_with("enron_message:"), "reservation row {n}: family key is not a message unit");
            ensure!(r.units.insert(unit.to_owned()), "reservation row {n}: the message is listed twice");
            *r.owner_rows.entry(owner.to_owned()).or_default() += 1;
            r.rows = n;
        }
        ensure!(r.owner_rows.len() == reserved.len(), "{} S0-9 reserved mailboxes have no listed message", reserved.len() - r.owner_rows.len());
        Ok(r)
    }
}

/// Render the listed messages of the reserved mailboxes with the Enron
/// adapter's rules into an adapter output directory (`report.json` written
/// only when every check passed, so a failed run cannot feed the cap or
/// dedup). `expect_archive_sha256`: the reservation scan's archive digest.
pub fn enron_reserved_render(archive: &Path, archive_url: &str, out: &Path, res: &EnronReservation, expect_archive_sha256: &str, inputs: &[Value]) -> Result<Value> {
    let params = json!({"archive": archive, "archive_url": archive_url, "bucket_key": "enron_mailbox:<opaque owner>", "mode": "reserved_diagnostic",
                        "rule": "B22: the reserved mailboxes (buckets 900..1000) rendered with the enron-v1 adapter's rules, the reservation listing's messages only"});
    let mut run = Run::start_sampled(out, "enron-reserved-diagnostic", ENRON_COLLECTION, params, None, Some(ENRON_RESERVED_BUCKETS))?;
    run.select_families(res.units.clone());
    for i in inputs {
        run.input(i.clone());
    }
    let extra = enron_with(archive, archive_url, &mut run, &EnronMode::ReservedDiagnostic(res))?;
    ensure!(extra["archive"]["sha256"] == expect_archive_sha256, "archive sha256 {} differs from the reservation scan's {expect_archive_sha256}", extra["archive"]["sha256"]);
    run.input(json!({"role": "enron_maildir_tar_gz", "path": archive, "sha256": extra["archive"]["sha256"], "bytes": extra["archive"]["bytes"]}));
    run.finish(extra)
}

/// Path names a reader must never open in round 2: old Dev-select, CAL and
/// test partitions and their round-2 successors, by name. Returns the rule
/// that matched. Tokens split on `/ - _ .`; a token starting `cal` or `test`
/// or equal to `select2` matches, so over-refusal is the failure mode.
pub fn restricted_path(p: &str) -> Option<&'static str> {
    let l = p.to_lowercase();
    for (pat, why) in [("dev-select", "dev-select"), ("dev_select", "dev_select"), ("preflight-r3", "preflight-r3"), ("frozen-partitions", "frozen-partitions"),
                       ("/test2-", "/test2-"), ("/cal2-", "/cal2-"), ("/select2-", "/select2-")] {
        if l.contains(pat) {
            return Some(why);
        }
    }
    for t in l.split(['/', '-', '_', '.']) {
        if t.starts_with("cal") {
            return Some("cal");
        }
        if t.starts_with("test") {
            return Some("test");
        }
        if t == "select2" {
            return Some("select2");
        }
    }
    None
}

/// `p` and the path it resolves to both pass `restricted_path`; returns the
/// canonical path. Resolving reads metadata only (symlinks are followed, no
/// target is opened), so a link with an unrestricted name that points at a
/// restricted file is refused before any read (amendment 23 item 23.5).
pub fn unrestricted_canonical(p: &Path, what: &str) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    if let Some(r) = restricted_path(&s) {
        bail!("{what} {s} matches the restricted name rule {r}; it is never read");
    }
    let c = std::fs::canonicalize(p).with_context(|| format!("{what} {s}"))?;
    if let Some(r) = restricted_path(&c.to_string_lossy()) {
        bail!("{what} {s} resolves to {} under the restricted name rule {r}; it is never read", c.display());
    }
    Ok(c)
}

/// A prior registry of the admission, as the reserved rows' prior
/// (amendment 23 item 23.1).
#[derive(Debug, Default)]
pub struct AdmissionPrior {
    /// The exact unrestricted file entries, each with its canonical path.
    pub kept: Vec<Value>,
    /// (label, rule) of the entries under a restricted name: never opened.
    pub dropped_restricted: Vec<(String, &'static str)>,
    /// Labels of the name-glob entries (directory walks): never walked.
    pub dropped_walks: Vec<String>,
}

/// Classify a prior registry's entries, relative paths resolved against
/// `root`: an entry whose given or resolved path matches `restricted_path`
/// is dropped unopened; a name-glob entry (a directory walk) is dropped
/// unwalked; every other entry must be an existing file whose canonical path
/// also passes, and is kept with that path.
pub fn admission_prior_entries(registry: &Value, root: &Path) -> Result<AdmissionPrior> {
    ensure!(registry["schema"] == crate::protected::REGISTRY_SCHEMA, "prior registry schema");
    let mut a = AdmissionPrior::default();
    for e in registry["entries"].as_array().context("prior registry entries")? {
        let label = e["label"].as_str().context("prior registry entry label")?.to_owned();
        let p = e["path"].as_str().with_context(|| format!("prior registry entry {label}: path"))?;
        let full = if Path::new(p).is_absolute() { PathBuf::from(p) } else { root.join(p) };
        if let Some(r) = restricted_path(p).or_else(|| restricted_path(&full.to_string_lossy())) {
            a.dropped_restricted.push((label, r));
            continue;
        }
        if e.get("names").is_some_and(|n| n.as_array().is_none_or(|v| !v.is_empty())) {
            a.dropped_walks.push(label);
            continue;
        }
        let c = unrestricted_canonical(&full, &format!("prior registry entry {label}"))?;
        ensure!(c.is_file(), "prior registry entry {label} ({}) is not a file", full.display());
        let mut k = e.clone();
        k["path"] = json!(c);
        a.kept.push(k);
    }
    Ok(a)
}

/// Candidate id -> prior-conflict reasons (`dedup::Options::prior_hits`).
pub type PriorHits = HashMap<String, BTreeSet<String>>;

/// Train-first short-window screen: a candidate of `collection` sharing any
/// folded `window`-word window with an admitted row of that collection is a
/// prior conflict (the admission's within-collection rule with every
/// admitted row ranked first). Returns candidate id -> reasons, and the
/// text-free hit rows.
pub fn admitted_window_hits(candidates: &[(String, String)], admitted: &[(String, String)], window: usize) -> Result<(PriorHits, Vec<Value>)> {
    ensure!(window >= 5, "window must be at least 5 words");
    let mut owner: HashMap<u64, usize> = HashMap::new();
    for (i, (_, t)) in admitted.iter().enumerate() {
        for w in text::shingles_folded_n(t, window) {
            owner.entry(w).or_insert(i);
        }
    }
    let reason = format!("shares_folded_{window}_word_window_with_admitted_row");
    let mut hits = HashMap::new();
    let mut rows = Vec::new();
    for (id, t) in candidates {
        let mut with: BTreeSet<&str> = BTreeSet::new();
        for w in text::shingles_folded_n(t, window) {
            if let Some(&i) = owner.get(&w) {
                with.insert(admitted[i].0.as_str());
            }
        }
        if !with.is_empty() {
            hits.insert(id.clone(), BTreeSet::from([reason.clone()]));
            rows.push(json!({"candidate_id": id, "reasons": [reason], "label": "admitted_pool_short_window", "admitted_ids": with}));
        }
    }
    Ok((hits, rows))
}

/// `STAGE0.json`'s schema.
pub const STAGE0_SCHEMA: &str = "slop_ninja_a_round2_stage0_v1";
/// The admission's supplementary (RAID) prior registry and the report of
/// its reverse scan, in the enron-v1 pool.
pub const ENRON_SUPPLEMENT_REGISTRY: &str = "admission/supplement-raid-v1.json";
pub const ENRON_SUPPLEMENT_SCAN: &str = "admission/supplement-raid-scan-report.json";

/// Stage 1 gate (protocol A2 section 15: the reserved mailboxes are rendered
/// at Stage 1): `STAGE0.json` must record Stage 0 closed. Returns its SHA-256.
pub fn stage0_closed(path: &Path) -> Result<String> {
    let b = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let v: Value = serde_json::from_slice(&b).with_context(|| format!("parse {}", path.display()))?;
    ensure!(v["schema"] == STAGE0_SCHEMA, "{} is not STAGE0.json", path.display());
    ensure!(v["closed"] == true, "{} records Stage 0 open: the reserved mailboxes are rendered only at Stage 1 (protocol A2 section 15)", path.display());
    Ok(text::sha_hex(&b))
}

/// Inputs of `enron_reserved`.
pub struct ReservedInputs<'a> {
    /// `VALUES-R2.json` (S0-9).
    pub values: &'a Path,
    /// `STAGE0.json`, which must record Stage 0 closed.
    pub stage0: &'a Path,
    /// The enron-v1 pool directory S0-9 binds.
    pub pool: &'a Path,
    /// The admission's other pools (EuroParl and NIH): exactly one per other
    /// source of the admission's dedup, each bound by that dedup's digest.
    pub also_pools: &'a [PathBuf],
    pub archive: &'a Path,
    pub archive_url: &'a str,
    /// The repository root, against which S0-9's input paths and the
    /// admission registries' relative paths resolve.
    pub root: &'a Path,
    /// Worker threads of the prior-index build (the index does not depend on
    /// them).
    pub threads: usize,
    pub out: &'a Path,
}

fn texts(path: &Path, collection: Option<&str>) -> Result<Vec<(String, String, Value)>> {
    let mut v = Vec::new();
    for r in crate::io::jsonl_rows(path)? {
        let r = r?;
        if collection.is_some_and(|c| r["collection"] != c) {
            continue;
        }
        let id = r["id"].as_str().with_context(|| format!("{}: row without id", path.display()))?.to_owned();
        let t = r["text"].as_str().with_context(|| format!("{}: row {id} without text", path.display()))?.to_owned();
        v.push((id, t, r));
    }
    Ok(v)
}

#[cfg(unix)]
fn private_tree(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let m = std::fs::symlink_metadata(p)?;
    if m.is_dir() {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?;
        for e in std::fs::read_dir(p)? {
            private_tree(&e?.path())?;
        }
    } else if m.is_file() {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn private_tree(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn private_dir(p: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700))?)
}
#[cfg(not(unix))]
fn private_dir(_: &Path) -> Result<()> {
    Ok(())
}

/// Run `body` with `out` a fresh directory made mode 700 before anything is
/// written below it. Afterwards, whether `body` succeeded or failed, every
/// directory below is 700 and every file 600, so a failed run leaves no
/// rendered text readable by others. `body`'s error comes first.
pub fn private_run<T>(out: &Path, body: impl FnOnce() -> Result<T>) -> Result<T> {
    crate::io::fresh_dir(out)?;
    private_dir(out)?;
    let r = body();
    let modes = private_tree(out);
    let v = r?;
    modes?;
    Ok(v)
}

/// B22: the reserved Enron mailboxes rendered under the enron-v1 admission's
/// rules as an ungated diagnostic, at Stage 1 only. Every input is verified
/// before the archive is opened: `STAGE0.json` closed, S0-9 (V23), the pool
/// (the one S0-9 names under `root`) and its `SHA256SUMS` against S0-9, the
/// reservation listing, the reservation scan's report (buckets, listing
/// digest, archive digest), the admission's cap and dedup reports, whose
/// recorded parameters are applied, the admission's other pools against its
/// dedup digests, its prior registries (amendment 23 items 23.1 and
/// 23.2), and the archive's digest. Then, in `out` (mode 700 from the
/// start): `rendered/` (the adapter on the listed messages), `capped/` (the
/// admission's contributor cap), `prior-index/` (a probe-filtered index of
/// the registry's kept files and the admitted pools), `supplement-scan/`
/// (the admission's reverse scan of its supplementary prior),
/// `window-hits.jsonl` (the Train-first short-window screen, item 23.3),
/// `prior-hits.jsonl` (both scans' hits), `dedup/` (the admission's dedup
/// parameters, no reserved-family rule: item 23.4) and
/// `RESERVED-DIAGNOSTIC.json`. No network, model, detector or GPU call;
/// Enron text never leaves the host.
pub fn enron_reserved(inp: &ReservedInputs) -> Result<Value> {
    let stage0_sha = stage0_closed(inp.stage0)?;
    let (halves, values_sha) = EnronHalves::from_values_file(inp.values)?;
    let pool_dir = unrestricted_canonical(inp.pool, "enron-v1 pool")?;
    let sums_rel = halves.inputs.get("SHA256SUMS").context("S0-9 binds no input named SHA256SUMS")?.0.clone();
    let named = std::fs::canonicalize(inp.root.join(&sums_rel)).with_context(|| format!("S0-9's input {sums_rel} under the root {}", inp.root.display()))?;
    ensure!(named == pool_dir.join("SHA256SUMS"), "the pool {} is not the one S0-9 names under the root ({sums_rel})", inp.pool.display());
    let pool = BoundPool::open(&pool_dir, Some(halves.input_sha("SHA256SUMS")?))?;
    let archive = unrestricted_canonical(inp.archive, "archive")?;
    let mut inputs = vec![json!({"role": "stage0", "path": inp.stage0, "sha256": stage0_sha, "closed": true}),
                          json!({"role": "values_r2", "path": inp.values, "sha256": values_sha}),
                          json!({"role": "enron_pool_sha256sums", "path": pool_dir.join("SHA256SUMS"), "sha256": pool.sums_sha256})];
    for name in ["ADMISSION.json", "metadata.jsonl", "reserved-metadata.jsonl"] {
        ensure!(pool.sha(name)? == halves.input_sha(name)?, "{name}: SHA256SUMS and S0-9 disagree");
    }
    let (adm_path, v) = pool.file("ADMISSION.json")?;
    inputs.push(v);
    let adm: Value = serde_json::from_slice(&std::fs::read(&adm_path)?)?;
    ensure!(adm["collection"] == ENRON_COLLECTION, "ADMISSION.json is not the {ENRON_COLLECTION} admission");
    ensure!(adm["counts"]["writer_keys"].as_u64() == Some(halves.admitted().len() as u64), "ADMISSION.json writer keys differ from S0-9's halves");
    let (res_path, v) = pool.file("reserved-metadata.jsonl")?;
    inputs.push(v);
    let res = EnronReservation::from_rows(crate::io::jsonl_rows(&res_path)?, &halves)?;
    ensure!(adm["counts"]["reserved_metadata_rows"].as_u64() == Some(res.rows), "ADMISSION.json reserved rows differ from the listing");

    let scan_name = pool.one("admission/scan-report-", ".json")?;
    let (scan_path, v) = pool.file(&scan_name)?;
    inputs.push(v);
    let scan: Value = serde_json::from_slice(&std::fs::read(&scan_path)?)?;
    ensure!(scan["adapter"] == "enron" && scan["complete"] == true && scan["candidates"] == 0, "{scan_name}: not a complete text-free Enron reservation scan");
    ensure!(scan["family_buckets"] == json!([ENRON_RESERVED_BUCKETS.0, ENRON_RESERVED_BUCKETS.1]), "{scan_name}: buckets are not {}..{}", ENRON_RESERVED_BUCKETS.0, ENRON_RESERVED_BUCKETS.1);
    ensure!(scan["metadata_only"]["sha256"] == pool.sha("reserved-metadata.jsonl")? && scan["metadata_only"]["rows"].as_u64() == Some(res.rows),
            "{scan_name}: its listing is not the pool's reserved-metadata.jsonl");
    let archive_sha = scan["inputs"].as_array().and_then(|a| a.iter().find(|i| i["role"] == "enron_maildir_tar_gz")).and_then(|i| i["sha256"].as_str())
        .with_context(|| format!("{scan_name}: no archive input"))?.to_owned();

    let cap_name = pool.one("admission/cap-report-", ".json")?;
    let (cap_path, v) = pool.file(&cap_name)?;
    inputs.push(v);
    let cap: Value = serde_json::from_slice(&std::fs::read(&cap_path)?)?;
    let cc = &cap["contributor_cap"];
    let max = cc["max_per_contributor"].as_u64().with_context(|| format!("{cap_name}: max_per_contributor"))? as usize;
    let seed = cc["seed"].as_str().with_context(|| format!("{cap_name}: seed"))?.to_owned();
    let per_mille = cc["document_sample_per_mille"].as_u64();
    ensure!(max >= 1 && cap["adapter"] == "enron", "{cap_name}: not the Enron contributor cap");

    let (dd_path, v) = pool.file("admission/dedup-report.json")?;
    inputs.push(v);
    let dd: Value = serde_json::from_slice(&std::fs::read(&dd_path)?)?;
    let o = &dd["options"];
    let strings = |k: &str| -> Result<Vec<String>> {
        o[k].as_array().with_context(|| format!("dedup-report options.{k}"))?.iter().map(|x| x.as_str().map(str::to_owned).context("string")).collect()
    };
    let near_window = o["near_window"].as_u64().context("dedup-report options.near_window")? as usize;
    let near_collections = strings("near_collections")?;
    let residue_collections = strings("residue_collections")?;
    let exclude_aliases = strings("exclude_aliases")?;
    let min_shared = dd["min_shared_sequences"].as_u64().context("dedup-report min_shared_sequences")? as usize;
    ensure!(near_collections.iter().any(|c| c == ENRON_COLLECTION), "dedup-report: the admission's short-window screen did not cover {ENRON_COLLECTION}");

    // The admission's dedup sources and their admitted rows (item 23.2):
    // enron-v1's documents, whose writers must be exactly S0-9's halves, and
    // one pool for each other source, each the dedup's output by digest.
    let by = dd["by_collection_sha256"].as_object().context("dedup-report by_collection_sha256")?;
    let sources: BTreeSet<String> = dd["sources"].as_array().context("dedup-report sources")?.iter()
        .map(|s| s["collection"].as_str().map(str::to_owned).context("dedup-report source collection")).collect::<Result<_>>()?;
    ensure!(sources.contains(ENRON_COLLECTION), "dedup-report: {ENRON_COLLECTION} is not one of its sources");
    ensure!(by.keys().cloned().collect::<BTreeSet<String>>() == sources, "dedup-report: by_collection_sha256 does not name exactly its sources");
    let docs_sha = |c: &str| -> Result<String> {
        by.get(c).and_then(|x| x["sha256"].as_str()).map(str::to_owned).with_context(|| format!("dedup-report by_collection_sha256.{c}.sha256"))
    };
    ensure!(pool.sha("documents.jsonl")? == docs_sha(ENRON_COLLECTION)?, "enron-v1 documents.jsonl is not the admission's dedup output for {ENRON_COLLECTION}");
    let (docs_path, v) = pool.file("documents.jsonl")?;
    inputs.push(v);
    let enron_rows = texts(&docs_path, None)?;
    let mut writers: BTreeSet<&str> = BTreeSet::new();
    for (id, _, r) in &enron_rows {
        ensure!(r["collection"] == ENRON_COLLECTION, "enron-v1 row {id} is not an Enron row");
        writers.insert(r["writer_key"].as_str().with_context(|| format!("enron-v1 row {id}: writer_key"))?);
    }
    ensure!(writers == halves.admitted(), "enron-v1 documents' writer keys are not exactly S0-9's two halves");
    let mut prior_files = vec![(docs_path.clone(), ENRON_COLLECTION.to_owned())];
    let mut others: BTreeMap<String, String> = BTreeMap::new();
    for c in sources.iter().filter(|c| c.as_str() != ENRON_COLLECTION) {
        others.insert(c.clone(), docs_sha(c)?);
    }
    let mut admitted_pools = Vec::new();
    for d in inp.also_pools {
        let dir = unrestricted_canonical(d, "admitted pool")?;
        let bp = BoundPool::open(&dir, None)?;
        let (ap, mut v) = bp.file("ADMISSION.json")?;
        let a: Value = serde_json::from_slice(&std::fs::read(&ap)?)?;
        let coll = a["collection"].as_str().with_context(|| format!("{}: ADMISSION.json collection", d.display()))?.to_owned();
        let want = others.remove(&coll).with_context(|| format!("pool {}: {coll} is not another source of the admission's dedup, or its pool is given twice", d.display()))?;
        v["role"] = json!("admitted_pool_admission");
        inputs.push(v);
        let (dp, mut v) = bp.file("documents.jsonl")?;
        ensure!(v["sha256"] == want.as_str(), "pool {}: documents.jsonl has sha256 {}, the admission's dedup output for {coll} is {want}", d.display(), v["sha256"]);
        v["role"] = json!("admitted_pool_documents");
        v["collection"] = json!(coll);
        inputs.push(v);
        admitted_pools.push(json!({"collection": coll, "path": dir, "documents_sha256": want, "sha256sums_sha256": bp.sums_sha256}));
        prior_files.push((dp, coll));
    }
    ensure!(others.is_empty(), "the admission's other sources need their pools (--also-pool): {} missing", others.keys().cloned().collect::<Vec<_>>().join(", "));

    // The admission's prior registry and its supplementary (RAID) registry
    // (item 23.1): exact unrestricted files only; the supplement's files
    // must be the ones the admission's reverse scan read.
    let reg_name = pool.one("admission/prior-registry-", ".json")?;
    let (reg_in, v) = pool.file(&reg_name)?;
    inputs.push(v);
    let prior = admission_prior_entries(&serde_json::from_slice(&std::fs::read(&reg_in)?)?, inp.root)?;
    let (sup_in, v) = pool.file(ENRON_SUPPLEMENT_REGISTRY)?;
    inputs.push(v);
    let (sup_scan_in, v) = pool.file(ENRON_SUPPLEMENT_SCAN)?;
    inputs.push(v);
    let sup_scan: Value = serde_json::from_slice(&std::fs::read(&sup_scan_in)?)?;
    ensure!(sup_scan["schema"] == crate::scan::REPORT_SCHEMA && sup_scan["registry_sha256"] == pool.sha(ENRON_SUPPLEMENT_REGISTRY)?,
            "{ENRON_SUPPLEMENT_SCAN}: not the admission's reverse scan of {ENRON_SUPPLEMENT_REGISTRY}");
    let sup = admission_prior_entries(&serde_json::from_slice(&std::fs::read(&sup_in)?)?, inp.root)?;
    ensure!(!sup.kept.is_empty() && sup.dropped_restricted.is_empty() && sup.dropped_walks.is_empty(), "{ENRON_SUPPLEMENT_REGISTRY}: expected exact unrestricted files only");
    let mut sup_files = Vec::new();
    for e in &sup.kept {
        let label = e["label"].as_str().unwrap_or("");
        let rec: Vec<&Value> = sup_scan["files"].as_array().into_iter().flatten().filter(|f| f["label"] == label).collect();
        ensure!(rec.len() == 1 && rec[0]["status"] == "scanned", "{ENRON_SUPPLEMENT_SCAN}: {label} was not scanned exactly once");
        let want = rec[0]["sha256"].as_str().with_context(|| format!("{ENRON_SUPPLEMENT_SCAN}: {label} sha256"))?;
        let p = PathBuf::from(e["path"].as_str().context("kept entry path")?);
        let (sha, bytes) = crate::io::file_sha(&p)?;
        ensure!(sha == want, "supplementary prior {label} has sha256 {sha}; the admission's reverse scan read {want}");
        sup_files.push(json!({"label": label, "path": p, "sha256": sha, "bytes": bytes}));
    }
    let (asha, _) = crate::io::file_sha(&archive)?;
    ensure!(asha == archive_sha, "archive {} has sha256 {asha}; the reservation scan read {archive_sha}", inp.archive.display());

    // Every input holds; open the archive.
    private_run(inp.out, || {
        let rendered = inp.out.join("rendered");
        let render = enron_reserved_render(&archive, inp.archive_url, &rendered, &res, &archive_sha, &inputs)?;
        let capped = inp.out.join("capped");
        let cap_out = forum::cap_and_sample(&rendered, &capped, max, &seed, per_mille)?;

        let mut entries: Vec<Value> = prior.kept.clone();
        entries.extend(prior_files.iter().map(|(p, c)| json!({"label": format!("admitted_pool:{c}"), "scope": "admitted_pool_documents", "path": p})));
        let reg_path = inp.out.join("prior-registry.json");
        crate::io::write_json(&reg_path, &json!({"schema": crate::protected::REGISTRY_SCHEMA, "registry_version": "b22-reserved-prior-v1",
                                                 "description": "amendment 23 items 23.1 and 23.2: the admission registry's exact unrestricted files and the admission's admitted pools", "entries": entries}))?;
        let probe = crate::protected::Probe::from_candidate_dirs(std::slice::from_ref(&capped))?;
        let index_dir = inp.out.join("prior-index");
        let index = crate::protected::build_with(&reg_path, Path::new("/"), &index_dir, Some(probe), inp.threads.max(1))?;

        let sup_reg = inp.out.join("supplement-registry.json");
        crate::io::write_json(&sup_reg, &json!({"schema": crate::protected::REGISTRY_SCHEMA, "registry_version": "b22-reserved-supplement-v1",
                                                "description": "amendment 23 item 23.1: the admission's supplementary prior, reverse-scanned as the admission did", "entries": sup.kept}))?;
        let sup_out = inp.out.join("supplement-scan");
        let sup_rep = crate::scan::run(&sup_reg, Path::new("/"), std::slice::from_ref(&capped), &sup_out)?;
        for (f, want) in sup_rep["files"].as_array().into_iter().flatten().zip(&sup_files) {
            ensure!(f["status"] == "scanned" && f["sha256"] == want["sha256"], "supplementary prior {} changed during the run", want["label"]);
        }
        ensure!(sup_rep["files_scanned"].as_u64() == Some(sup_files.len() as u64), "supplementary prior: {} files scanned, {} bound", sup_rep["files_scanned"], sup_files.len());

        let cands: Vec<(String, String)> = texts(&capped.join("candidates.jsonl"), Some(ENRON_COLLECTION))?.into_iter().map(|(i, t, _)| (i, t)).collect();
        let admitted: Vec<(String, String)> = enron_rows.into_iter().map(|(i, t, _)| (i, t)).collect();
        let (mut hits, hit_rows) = admitted_window_hits(&cands, &admitted, near_window)?;
        let win_path = inp.out.join("window-hits.jsonl");
        let mut hw = crate::io::JsonlWriter::create(&win_path)?;
        for r in &hit_rows {
            hw.write(r)?;
        }
        let (win_sha, _) = hw.finish()?;
        let hits_path = inp.out.join("prior-hits.jsonl");
        let mut pw = crate::io::JsonlWriter::create(&hits_path)?;
        for r in &hit_rows {
            pw.write(r)?;
        }
        for r in crate::io::jsonl_rows(&sup_out.join("hits.jsonl"))? {
            pw.write(&r?)?;
        }
        let (hits_sha, _) = pw.finish()?;
        let sup_hits = crate::scan::load_hits(&sup_out.join("hits.jsonl"))?;
        let sup_hit_candidates = sup_hits.len();
        for (id, rs) in sup_hits {
            hits.entry(id).or_default().extend(rs);
        }
        let opt = crate::dedup::Options {
            near_window, near_collections, residue_collections, exclude_aliases,
            prior_hits: Some(hits), prior_hits_file: Some((hits_path.clone(), hits_sha.clone())),
            reserved_families: BTreeSet::new(), reserved_files: Vec::new(),
        };
        let dedup_dir = inp.out.join("dedup");
        let dedup = crate::dedup::run_with(&index_dir, std::slice::from_ref(&capped), &dedup_dir, min_shared, &opt)?;

        // The diagnostic set: every row a listed reserved mailbox's, marked
        // diagnostic only and never hosted.
        let reserved: BTreeSet<&str> = halves.reserved.iter().map(String::as_str).collect();
        let mut by_owner: BTreeMap<String, u64> = BTreeMap::new();
        let mut rows = 0u64;
        for r in crate::io::jsonl_rows(&dedup_dir.join("admitted.jsonl"))? {
            let r = r?;
            let w = r["writer_key"].as_str().unwrap_or("");
            ensure!(r["collection"] == ENRON_COLLECTION && reserved.contains(w), "diagnostic row {} is not a reserved mailbox's Enron row", r["id"]);
            ensure!(r["rights"]["evaluation_use"] == "diagnostic_only" && r["rights"]["hosted_model_use"] == "never" && r["rights"]["train_use"] == false,
                    "diagnostic row {} is not marked diagnostic only and never hosted", r["id"]);
            *by_owner.entry(w.to_owned()).or_default() += 1;
            rows += 1;
        }
        let (adm_sha, _) = crate::io::file_sha(&dedup_dir.join("admitted.jsonl"))?;
        let report = json!({
            "schema": ENRON_RESERVED_SCHEMA,
            "finished_utc": crate::io::now_utc(),
            "rule": "protocol A2 section 5 (Diagnostics), V23, B22 and amendment 23: the S0-9 reserved mailboxes (buckets 900..1000) rendered at Stage 1 with the enron-v1 admission's adapter, contributor cap and dedup parameters as an ungated FPR diagnostic; never trained on, never sent to a hosted model",
            "values": {"path": inp.values, "sha256": values_sha, "reserved_list_sha256": key_list_sha256(&halves.reserved), "reserved_mailboxes": halves.reserved.len()},
            "root": inp.root,
            "inputs": inputs,
            "reservation": {"listed_messages": res.rows, "listed_mailboxes": res.owner_rows.len()},
            "archive_sha256": archive_sha,
            "prior": {
                "rule": "amendment 23 item 23.1: the admission registry's exact files whose given and resolved paths pass the restricted-name rule, read as they stand; restricted entries dropped unopened, name-glob (directory walk) entries dropped unwalked; plus the admitted pools of item 23.2",
                "registry": {"file": reg_name, "sha256": pool.sha(&reg_name)?},
                "kept": prior.kept.iter().map(|e| json!({"label": e["label"], "scope": e["scope"]})).collect::<Vec<_>>(),
                "dropped_restricted": prior.dropped_restricted.iter().map(|(l, r)| json!({"label": l, "rule": r})).collect::<Vec<_>>(),
                "dropped_walks": prior.dropped_walks,
                "admitted_pools": admitted_pools,
                "files": index["files"].as_array().map(|a| a.iter().map(|f| json!({"label": f["label"], "path": f["path"], "sha256": f["sha256"], "bytes": f["bytes"], "records": f["records"]})).collect::<Vec<_>>()),
            },
            "supplement": {"rule": "amendment 23 item 23.1: the admission's supplementary prior, each file the one its reverse scan read, reverse-scanned over the capped reserved rows; hits are prior conflicts",
                           "registry": {"file": ENRON_SUPPLEMENT_REGISTRY, "sha256": pool.sha(ENRON_SUPPLEMENT_REGISTRY)?},
                           "admission_scan": {"file": ENRON_SUPPLEMENT_SCAN, "sha256": pool.sha(ENRON_SUPPLEMENT_SCAN)?},
                           "files": sup_files, "candidates_with_hits": sup_hit_candidates, "hits_sha256": sup_rep["hits"]["sha256"]},
            "steps": {
                "render": {"dir": "rendered", "report_sha256": crate::io::file_sha(&rendered.join("report.json"))?.0, "candidates": render["candidates"], "rejection_reasons": render["rejection_reasons"]},
                "cap": {"dir": "capped", "max_per_contributor": max, "seed": seed, "document_sample_per_mille": per_mille, "kept": cap_out["kept"], "removed": cap_out["removed"]},
                "prior_index": {"dir": "prior-index", "registry_sha256": crate::io::file_sha(&reg_path)?.0, "files_scanned": index["files_scanned"], "threads": inp.threads.max(1)},
                "supplement_scan": {"dir": "supplement-scan", "registry_sha256": crate::io::file_sha(&sup_reg)?.0, "files_scanned": sup_rep["files_scanned"], "candidates_with_hits": sup_rep["candidates_with_hits"]},
                "window_screen": {"file": "window-hits.jsonl", "sha256": win_sha, "window": near_window, "candidates_hit": hit_rows.len(),
                                  "rule": "amendment 23 item 23.3: a reserved candidate sharing any folded window of the admission's near_window words with any admitted enron-v1 row is a prior conflict (admitted rows rank first)"},
                "prior_hits": {"file": "prior-hits.jsonl", "sha256": hits_sha, "rule": "the window screen's and the supplement scan's hits, as dedup prior conflicts"},
                "dedup": {"dir": "dedup", "admitted": dedup["admitted"], "excluded": dedup["excluded"], "families_excluded_for_prior_conflict": dedup["families_excluded_for_prior_conflict"],
                          "min_shared": min_shared, "near_window": near_window,
                          "reserved_family_rule": "amendment 23 item 23.4: not applied; the reservation listing is this population"},
            },
            "diagnostic": {"file": "dedup/admitted.jsonl", "rows": rows, "sha256": adm_sha, "mailboxes_with_rows": by_owner.len(), "rows_per_mailbox": by_owner.values().collect::<Vec<_>>()},
            "calls": {"network": 0, "hosted_model": 0, "detector": 0, "gpu": 0},
            "limits": ["Historical email is a human proxy; an assistant may have typed a message from the owner's account.",
                       "Overlap protection covers the admission registry's kept exact files, the admitted pools and the supplementary prior only; the registry's restricted and directory-walk entries are not screened.",
                       "Pretraining exposure of the encoder is unknown.",
                       "Ungated: reported as an FPR diagnostic, never a gate, selector or calibration input."],
        });
        crate::io::write_json(&inp.out.join("RESERVED-DIAGNOSTIC.json"), &report)?;
        Ok(report)
    })
}

/// The R0 out-of-domain human selection's email pool restricted to the
/// diagnostic-half mailboxes (protocol A2 section 5; r2 F8). `selection`:
/// `ood/selection.json`; `meta`: `ood/meta.jsonl` rows (`id`, `pool`,
/// `writer_key`; never text), in selection order. Each pool's ids must match
/// the selection's count and `ids_sha256` (ids joined by newline), the
/// email pool's `documents_sha256` must be the S0-9-bound enron-v1
/// `documents.jsonl`, and every email row's writer must be an admitted
/// mailbox. Email rows of Train-half mailboxes are dropped; other pools pass
/// unchanged. Returns the kept rows (selection order) and a summary.
pub fn ood_email_restriction(halves: &EnronHalves, selection: &Value, meta: impl IntoIterator<Item = Result<Value>>, enron_documents_sha256: &str) -> Result<(Vec<Value>, Value)> {
    ensure!(selection["schema"] == OOD_SELECTION_SCHEMA, "not the OOD human diagnostic selection");
    let pools = selection["pools"].as_object().context("selection pools")?;
    let e = pools.get(OOD_ENRON_POOL).with_context(|| format!("selection has no {OOD_ENRON_POOL} pool"))?;
    ensure!(e["documents_sha256"] == enron_documents_sha256, "the selection's {OOD_ENRON_POOL} documents are not the S0-9-bound enron-v1 documents.jsonl");
    let train: BTreeSet<&str> = halves.train.iter().map(String::as_str).collect();
    let diag: BTreeSet<&str> = halves.diagnostic.iter().map(String::as_str).collect();
    let mut ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut kept = Vec::new();
    let (mut dropped, mut kept_email) = (0u64, Vec::new());
    let mut kept_writers: BTreeSet<String> = BTreeSet::new();
    for row in meta {
        let row = row?;
        ensure!(row.get("text").is_none(), "OOD meta rows never carry text");
        let pool = row["pool"].as_str().context("meta pool")?.to_owned();
        let id = row["id"].as_str().context("meta id")?.to_owned();
        ensure!(pools.contains_key(&pool), "meta row {id}: pool {pool} is not in the selection");
        ids.entry(pool.clone()).or_default().push(id.clone());
        if pool != OOD_ENRON_POOL {
            kept.push(row);
            continue;
        }
        let w = row["writer_key"].as_str().with_context(|| format!("meta row {id}: writer_key"))?;
        if diag.contains(w) {
            kept_writers.insert(w.to_owned());
            kept_email.push(id);
            kept.push(row);
        } else {
            ensure!(train.contains(w), "meta row {id}: its writer is in neither S0-9 half");
            dropped += 1;
        }
    }
    let mut per_pool = serde_json::Map::new();
    for (name, p) in pools {
        let got = ids.get(name).map(Vec::as_slice).unwrap_or(&[]);
        ensure!(p["taken"].as_u64() == Some(got.len() as u64), "pool {name}: meta holds {} rows, the selection took {}", got.len(), p["taken"]);
        ensure!(p["ids_sha256"] == text::sha_hex(got.join("\n").as_bytes()).as_str(), "pool {name}: meta ids do not match the selection's ids_sha256");
        let after = if name == OOD_ENRON_POOL { kept_email.len() } else { got.len() };
        per_pool.insert(name.clone(), json!({"selected": got.len(), "kept": after}));
    }
    let summary = json!({
        "pools": per_pool,
        "email": {"pool": OOD_ENRON_POOL, "selected": ids.get(OOD_ENRON_POOL).map_or(0, Vec::len), "kept": kept_email.len(), "dropped_train_half": dropped,
                  "kept_writers": kept_writers.len(), "kept_ids_sha256": text::sha_hex(kept_email.join("\n").as_bytes()),
                  "diagnostic_half_list_sha256": key_list_sha256(&halves.diagnostic)},
        "rule": "protocol A2 section 5 (r2 F8): the email OOD pool keeps only rows whose writer is in S0-9's diagnostic half; parliament and grant-abstract pools are unchanged",
    });
    Ok((kept, summary))
}

/// `ood_email_restriction` from files: writes `out/meta-restricted.jsonl`
/// (ids, pools and writer keys; never text) and `out/restriction.json`.
pub fn enron_ood_restrict(values: &Path, pool: &Path, selection: &Path, meta: &Path, out: &Path) -> Result<Value> {
    let (halves, values_sha) = EnronHalves::from_values_file(values)?;
    let bp = BoundPool::open(pool, Some(halves.input_sha("SHA256SUMS")?))?;
    let sel_b = std::fs::read(selection)?;
    let sel: Value = serde_json::from_slice(&sel_b)?;
    let (kept, summary) = ood_email_restriction(&halves, &sel, crate::io::jsonl_rows(meta)?, bp.sha("documents.jsonl")?)?;
    private_run(out, || {
        let mut w = crate::io::JsonlWriter::create(&out.join("meta-restricted.jsonl"))?;
        for r in &kept {
            w.write(r)?;
        }
        let (sha, n) = w.finish()?;
        let report = json!({
            "schema": OOD_EMAIL_SCHEMA,
            "finished_utc": crate::io::now_utc(),
            "inputs": [{"role": "values_r2", "path": values, "sha256": values_sha},
                       {"role": "enron_pool_sha256sums", "path": pool.join("SHA256SUMS"), "sha256": bp.sums_sha256},
                       {"role": "ood_selection", "path": selection, "sha256": text::sha_hex(&sel_b)},
                       {"role": "ood_meta", "path": meta, "sha256": crate::io::file_sha(meta)?.0}],
            "output": {"file": "meta-restricted.jsonl", "rows": n, "sha256": sha},
            "restriction": summary,
            "calls": {"network": 0, "hosted_model": 0, "detector": 0, "gpu": 0},
            "limits": ["Restricts R0's existing selection; it draws no new row.", "Text is never read: ids, pools and writer keys only."],
        });
        crate::io::write_json(&out.join("restriction.json"), &report)?;
        Ok(report)
    })
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

    // ------------------------------------------------ B22 (synthetic only)

    mod b22 {
        use super::super::*;
        use crate::adapter::{self, Run};
        use std::path::{Path, PathBuf};

        /// Letters-only unique token for `n`.
        fn tag(n: usize) -> String {
            let mut s = String::from("zq");
            let mut x = n;
            loop {
                s.push((b'a' + (x % 26) as u8) as char);
                x /= 26;
                if x == 0 {
                    break;
                }
            }
            s
        }

        fn para(seed: usize) -> String {
            (0..7).map(|k| {
                let u = |j: usize| tag(((seed * 10 + k) * 4) + j);
                format!("We should review the {} plan with the {} group before we sign the {} contract this week.", u(0), u(1), u(2))
            }).collect::<Vec<_>>().join(" ")
        }

        /// The excerpt the Enron adapter takes from `body(seed)`.
        fn excerpt(seed: usize) -> String {
            format!("Dear team,\n\n{}\n\n{}", para(seed * 2), para(seed * 2 + 1))
        }

        fn body(seed: usize) -> String {
            format!("Dear team,\n\n{}\n\n{}\n\nThanks,\nPat\n\n-----Original Message-----\nFrom: someone\nold quoted text", para(seed * 2), para(seed * 2 + 1))
        }

        fn message_with(owner: &str, mid: usize, from_owner: bool, body: &str) -> String {
            let sur = owner.split('-').next().unwrap();
            let from = if from_owner { format!("{sur}.x@example.com") } else { "assistant.y@example.com".to_owned() };
            format!("Message-ID: <{mid}.{owner}@example.com>\nDate: Thu, 7 Jun 2001 06:51:25 -0700 (PDT)\nFrom: {from}\nTo: team@example.com\nSubject: note {mid}\nContent-Type: text/plain; charset=us-ascii\n\n{body}")
        }

        fn message(owner: &str, seed: usize, from_owner: bool) -> String {
            message_with(owner, seed, from_owner, &body(seed))
        }

        struct Owner {
            dir: String,
            key: String,
        }

        /// `n` synthetic mailbox owners whose keys satisfy `want(bucket)`.
        fn owners(n: usize, start: usize, want: impl Fn(u32) -> bool) -> Vec<Owner> {
            let mut v = Vec::new();
            let mut i = start;
            while v.len() < n {
                let dir = format!("mailer{i}-k");
                let key = contributor_key("enron_mailbox", &dir);
                if want(adapter::bucket(&key)) {
                    v.push(Owner { dir, key });
                }
                i += 1;
            }
            v
        }

        fn archive(path: &Path, msgs: &[(String, &str, String, String)]) {
            let f = std::fs::File::create(path).unwrap();
            let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            for (owner, folder, file, raw) in msgs {
                let mut h = tar::Header::new_gnu();
                h.set_size(raw.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, format!("maildir/{owner}/{folder}/{file}"), raw.as_bytes()).unwrap();
            }
            b.into_inner().unwrap().finish().unwrap();
        }

        /// A synthetic S0-9 record over `admitted` (V23 order) and `reserved`.
        fn s09(admitted: &[String], reserved: &[String], inputs: Value) -> Value {
            let mut order = admitted.to_vec();
            order.sort_by_key(|k| enron_half_plan_key(k));
            let (t, d) = order.split_at(ENRON_TRAIN_HALF_KEYS);
            let mut r = reserved.to_vec();
            r.sort();
            json!({"id": "S0-9", "name": "enron_halves", "plan_key_prefix": ENRON_HALF_PLAN_PREFIX,
                   "train_half": {"writer_keys_in_plan_key_order": t, "writer_keys": t.len(), "list_sha256": key_list_sha256(t)},
                   "diagnostic_half": {"writer_keys_in_plan_key_order": d, "writer_keys": d.len(), "list_sha256": key_list_sha256(d)},
                   "reserved_mailboxes_diagnostic": {"owner_keys_sorted": r, "owner_keys": r.len(), "list_sha256": key_list_sha256(&r)},
                   "checks": {"writer_keys": order.len(), "reserved_owner_keys": r.len()}, "inputs": inputs})
        }

        fn keys(o: &[Owner]) -> Vec<String> {
            o.iter().map(|x| x.key.clone()).collect()
        }

        fn refix(v: &mut Value, sec: &str, field: &str) {
            let k: Vec<String> = v[sec][field].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_owned()).collect();
            v[sec]["list_sha256"] = json!(key_list_sha256(&k));
        }

        fn err(r: Result<impl std::fmt::Debug>) -> String {
            format!("{:#}", r.expect_err("expected a refusal"))
        }

        #[test]
        fn halves_verify_v23_order_digests_and_buckets() {
            let adm = keys(&owners(53, 0, |b| b < 900));
            let res = keys(&owners(3, 0, |b| b >= 900));
            let v = s09(&adm, &res, json!([]));
            let h = EnronHalves::from_s09(&v).unwrap();
            assert_eq!((h.train.len(), h.diagnostic.len(), h.reserved.len()), (50, 3, 3));
            assert_eq!(h.admitted().len(), 53);
            // Swapped halves: V23's order fails.
            let mut m = v.clone();
            let (a, b) = (m["train_half"]["writer_keys_in_plan_key_order"][0].clone(), m["diagnostic_half"]["writer_keys_in_plan_key_order"][0].clone());
            m["train_half"]["writer_keys_in_plan_key_order"][0] = b;
            m["diagnostic_half"]["writer_keys_in_plan_key_order"][0] = a;
            refix(&mut m, "train_half", "writer_keys_in_plan_key_order");
            refix(&mut m, "diagnostic_half", "writer_keys_in_plan_key_order");
            assert!(err(EnronHalves::from_s09(&m)).contains("plan-key order"));
            // A tampered list digest.
            let mut m = v.clone();
            m["diagnostic_half"]["list_sha256"] = json!("00");
            assert!(err(EnronHalves::from_s09(&m)).contains("list_sha256"));
            // A reserved key outside 900..1000.
            let mut m = v.clone();
            let low = owners(1, 1000, |b| b < 900).remove(0).key;
            let mut r = res.clone();
            r.push(low);
            r.sort();
            m["reserved_mailboxes_diagnostic"]["owner_keys_sorted"] = json!(r);
            m["reserved_mailboxes_diagnostic"]["owner_keys"] = json!(r.len());
            m["checks"]["reserved_owner_keys"] = json!(r.len());
            refix(&mut m, "reserved_mailboxes_diagnostic", "owner_keys_sorted");
            assert!(err(EnronHalves::from_s09(&m)).contains("outside 900..1000"));
            // An admitted key inside the reserved buckets.
            let mut adm2 = adm.clone();
            adm2[0] = owners(1, 1000, |b| b >= 900).remove(0).key;
            assert!(err(EnronHalves::from_s09(&s09(&adm2, &res, json!([])))).contains("reserved buckets"));
            // Unsorted reserved keys.
            let mut m = v.clone();
            let mut r = res.clone();
            r.sort();
            r.reverse();
            m["reserved_mailboxes_diagnostic"]["owner_keys_sorted"] = json!(r);
            refix(&mut m, "reserved_mailboxes_diagnostic", "owner_keys_sorted");
            assert!(err(EnronHalves::from_s09(&m)).contains("sorted"));
            // A Train half that is not 50 keys.
            let mut m = v.clone();
            let t: Vec<Value> = m["train_half"]["writer_keys_in_plan_key_order"].as_array().unwrap()[..49].to_vec();
            m["train_half"]["writer_keys_in_plan_key_order"] = json!(t);
            m["train_half"]["writer_keys"] = json!(49);
            refix(&mut m, "train_half", "writer_keys_in_plan_key_order");
            assert!(err(EnronHalves::from_s09(&m)).contains("first 50"));
            // Counts that disagree with the lists.
            let mut m = v.clone();
            m["checks"]["writer_keys"] = json!(52);
            assert!(err(EnronHalves::from_s09(&m)).contains("checks.writer_keys"));
            let mut m = v.clone();
            m["checks"]["reserved_owner_keys"] = json!(4);
            assert!(err(EnronHalves::from_s09(&m)).contains("checks.reserved_owner_keys"));
        }

        #[test]
        fn train_render_refuses_reserved_buckets_unless_metadata_only() {
            assert!(enron_train_buckets(Some((0, 900)), false).is_ok());
            assert!(enron_train_buckets(Some((100, 200)), false).is_ok());
            assert!(enron_train_buckets(Some((900, 1000)), true).is_ok());
            assert!(enron_train_buckets(None, true).is_ok());
            assert!(enron_train_buckets(Some((0, 901)), false).is_err());
            assert!(enron_train_buckets(Some((900, 1000)), false).is_err());
            assert!(enron_train_buckets(None, false).is_err());
        }

        #[test]
        fn train_adapter_never_renders_reserved_prose() {
            let f = fixture(&[1]);
            // The library refuses on its own (the CLI's range check is a second guard): a Train
            // run whose range holds a reserved mailbox stops before that message is read.
            for (i, b) in [Some((900, 1000)), None, Some((0, 1000))].into_iter().enumerate() {
                let mut run = Run::start_sampled(&f.dir.path().join(format!("t{i}")), "enron", ENRON_COLLECTION, json!({}), None, b).unwrap();
                let e = err(enron(&f.archive, "u", &mut run));
                assert!(e.contains("metadata-only listing"), "{b:?}: {e}");
            }
            // Train buckets render the admitted owners only.
            let out = f.dir.path().join("train");
            let mut run = Run::start_sampled(&out, "enron", ENRON_COLLECTION, json!({}), None, Some((0, 900))).unwrap();
            let x = enron(&f.archive, "u", &mut run).unwrap();
            run.finish(x).unwrap();
            let c = rows(&out.join("candidates.jsonl"));
            assert_eq!(c.len(), 3);
            assert!(c.iter().all(|r| f.adm.iter().any(|o| r["writer_key"] == o.key.as_str())));
            // A metadata-only listing of the reserved buckets is allowed (the fixture's scan ran it).
            assert_eq!(rows(&f.scan.join("reserved-metadata.jsonl")).len(), 2);
        }

        #[test]
        fn restricted_names_and_admission_prior_entries() {
            for p in ["/d/eval-humans/work/dedup/dev-select/admitted.jsonl", "/d/x/dev_select.jsonl", "/d/eval-humans/work/dedup/cal/admitted.jsonl",
                      "/d/rc-work/calibration/rows.jsonl", "/d/prior-registry-tierc-test-v1.json", "/d/r2/test2-carve/x.jsonl", "/d/r2/select2-x/y",
                      "/d/preflight-r3/a.json", "/d/frozen-partitions/p.json", "/d/x/cal2-mock/rows.jsonl", "/d/TEST/rows.jsonl"] {
                assert!(restricted_path(p).is_some(), "{p}");
            }
            for p in ["/d/data/pools/enron-v1/documents.jsonl", "/d/splits-v5/train/documents.jsonl", "/d/pools/nih-exporter-v1/documents.jsonl", "/d/local/latest.jsonl"] {
                assert_eq!(restricted_path(p), None, "{p}");
            }
            let d = tempfile::tempdir().unwrap();
            let root = d.path();
            std::fs::create_dir_all(root.join("a/ev/dev-select")).unwrap();
            for f in ["a/train.jsonl", "a/train2.jsonl", "a/ev/dev-select/x.jsonl"] {
                std::fs::write(root.join(f), "{}\n").unwrap();
            }
            std::os::unix::fs::symlink(root.join("a/ev/dev-select/x.jsonl"), root.join("a/link.jsonl")).unwrap();
            std::os::unix::fs::symlink(root.join("a/ev/dev-select"), root.join("pool-link")).unwrap();
            let reg = |entries: Value| json!({"schema": crate::protected::REGISTRY_SCHEMA, "entries": entries});
            let a = admission_prior_entries(&reg(json!([
                {"label": "t", "scope": "train", "path": "a/train.jsonl"},
                {"label": "ds", "path": "a/ev/dev-select/x.jsonl"},
                {"label": "cal", "path": "a/missing/cal/x.jsonl"},
                {"label": "walk", "path": "a", "names": ["*.jsonl"]},
                {"label": "abs", "path": root.join("a/train2.jsonl")},
                {"label": "empty-names", "path": "a/train.jsonl", "names": []}])), root).unwrap();
            let labels: Vec<&str> = a.kept.iter().map(|e| e["label"].as_str().unwrap()).collect();
            assert_eq!(labels, ["t", "abs", "empty-names"]);
            assert_eq!(a.kept[0]["path"], json!(std::fs::canonicalize(root.join("a/train.jsonl")).unwrap()));
            assert_eq!(a.kept[0]["scope"], "train");
            assert_eq!(a.dropped_restricted, vec![("ds".to_owned(), "dev-select"), ("cal".to_owned(), "cal")]);
            assert_eq!(a.dropped_walks, ["walk"]);
            // A link with an unrestricted name to a restricted file or directory is refused.
            assert!(err(admission_prior_entries(&reg(json!([{"label": "l", "path": "a/link.jsonl"}])), root)).contains("resolves to"));
            assert!(err(unrestricted_canonical(&root.join("pool-link"), "admitted pool")).contains("resolves to"));
            assert!(err(unrestricted_canonical(&root.join("a/ev/dev-select"), "admitted pool")).contains("restricted name rule dev-select"));
            // A missing exact file and a directory without name globs.
            assert!(err(admission_prior_entries(&reg(json!([{"label": "m", "path": "a/none.jsonl"}])), root)).contains("prior registry entry m"));
            assert!(err(admission_prior_entries(&reg(json!([{"label": "dir", "path": "a"}])), root)).contains("not a file"));
            assert!(err(admission_prior_entries(&json!({"schema": "x", "entries": []}), root)).contains("schema"));
        }

        #[test]
        fn private_run_leaves_a_failed_run_private() {
            use std::os::unix::fs::PermissionsExt;
            let d = tempfile::tempdir().unwrap();
            let out = d.path().join("o");
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            // An empty, world-readable directory as a default umask would leave it.
            std::fs::create_dir(&out).unwrap();
            std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755)).unwrap();
            let r: Result<()> = private_run(&out, || {
                assert_eq!(mode(&out), 0o700, "private before anything is written");
                std::fs::create_dir_all(out.join("rendered"))?;
                std::fs::write(out.join("rendered/candidates.jsonl"), "{}\n")?;
                std::fs::set_permissions(out.join("rendered"), std::fs::Permissions::from_mode(0o755))?;
                std::fs::set_permissions(out.join("rendered/candidates.jsonl"), std::fs::Permissions::from_mode(0o644))?;
                bail!("late failure")
            });
            assert!(err(r).contains("late failure"));
            assert_eq!((mode(&out), mode(&out.join("rendered")), mode(&out.join("rendered/candidates.jsonl"))), (0o700, 0o700, 0o600));
            assert!(err(private_run(&out, || Ok(()))).contains("not empty"));
        }

        /// Archive and reservation fixture. `bodies[i]`: reserved owner i's
        /// sent messages as (message number, body); reserved owner 0 also has
        /// one inbox message and one assistant-sent message, and three
        /// admitted owners have one sent message each.
        struct Fixture {
            dir: tempfile::TempDir,
            archive: PathBuf,
            adm: Vec<Owner>,
            res: Vec<Owner>,
            /// (owner index, message number) of every reserved sent message.
            reserved_msgs: Vec<(usize, usize)>,
            scan: PathBuf,
        }

        fn fixture_with(bodies: &[Vec<(usize, String)>]) -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let adm = owners(51, 0, |b| b < 900);
            let res = owners(bodies.len(), 0, |b| b >= 900);
            let mut msgs = Vec::new();
            let mut reserved_msgs = Vec::new();
            for (i, o) in res.iter().enumerate() {
                for (m, (mid, b)) in bodies[i].iter().enumerate() {
                    msgs.push((o.dir.clone(), "sent", format!("{m}."), message_with(&o.dir, *mid, true, b)));
                    reserved_msgs.push((i, *mid));
                }
            }
            // Same owner, not a sent folder; and a sent message from another address.
            msgs.push((res[0].dir.clone(), "inbox", "1.".into(), message(&res[0].dir, 900, true)));
            msgs.push((res[0].dir.clone(), "sent_items", "9.".into(), message(&res[0].dir, 901, false)));
            for (j, o) in adm.iter().take(3).enumerate() {
                msgs.push((o.dir.clone(), "sent", "1.".into(), message(&o.dir, 500 + j, true)));
            }
            let archive_path = dir.path().join("enron.tar.gz");
            archive(&archive_path, &msgs);
            // The reservation scan, exactly as the admission ran it.
            let scan = dir.path().join("scan");
            let mut run = Run::start_sampled(&scan, "enron", ENRON_COLLECTION, json!({}), None, Some(ENRON_RESERVED_BUCKETS)).unwrap();
            run.enable_metadata_only().unwrap();
            let extra = enron(&archive_path, "https://example.invalid/enron.tar.gz", &mut run).unwrap();
            run.input(json!({"role": "enron_maildir_tar_gz", "path": archive_path, "sha256": extra["archive"]["sha256"], "bytes": extra["archive"]["bytes"]}));
            run.finish(extra).unwrap();
            Fixture { dir, archive: archive_path, adm, res, reserved_msgs, scan }
        }

        /// `per[i]` seeded messages for reserved owner i (seeds from 100).
        fn fixture(per: &[usize]) -> Fixture {
            let mut seed = 100;
            let bodies: Vec<Vec<(usize, String)>> = per.iter().map(|&n| (0..n).map(|_| {
                seed += 1;
                (seed - 1, body(seed - 1))
            }).collect()).collect();
            fixture_with(&bodies)
        }

        fn halves_of(f: &Fixture) -> EnronHalves {
            EnronHalves::from_s09(&s09(&keys(&f.adm), &keys(&f.res), json!([]))).unwrap()
        }

        fn rows(p: &Path) -> Vec<Value> {
            crate::io::jsonl_rows(p).unwrap().map(Result::unwrap).collect()
        }

        fn archive_sha(f: &Fixture) -> String {
            crate::io::file_sha(&f.archive).unwrap().0
        }

        /// The message number of a rendered row.
        fn mid(r: &Value) -> usize {
            r["provenance"]["message_id"].as_str().unwrap().trim_start_matches('<').split('.').next().unwrap().parse().unwrap()
        }

        #[test]
        fn reservation_listing_is_checked_against_s09() {
            let f = fixture(&[2, 1]);
            let h = halves_of(&f);
            let listing = rows(&f.scan.join("reserved-metadata.jsonl"));
            // Every sent message of the reserved mailboxes is listed, the assistant-sent one too
            // (the listing precedes screening); the inbox message is not.
            assert_eq!(listing.len(), 4);
            let r = EnronReservation::from_rows(listing.iter().cloned().map(Ok), &h).unwrap();
            assert_eq!((r.rows, r.units.len(), r.owner_rows.len()), (4, 4, 2));
            let bad = |edit: &dyn Fn(&mut Vec<Value>), want: &str| {
                let mut l = listing.clone();
                edit(&mut l);
                let e = err(EnronReservation::from_rows(l.into_iter().map(Ok), &h));
                assert!(e.contains(want), "{e}");
            };
            bad(&|l| l[0]["prose_opened"] = json!(true), "prose");
            bad(&|l| l[0]["text"] = json!("x"), "prose");
            bad(&|l| l[0]["bucket"] = json!(950), "bucket");
            bad(&|l| l[0]["group_key"] = json!("enron_mailbox:00000000000000000000"), "owner");
            bad(&|l| {
                let x = l[0].clone();
                l.push(x)
            }, "twice");
            bad(&|l| l.retain(|r| r["owner_key"] != f.res[1].key.as_str()), "no listed message");
            bad(&|l| l[0]["schema"] = json!("x"), "metadata-only");
            bad(&|l| l[0]["family_key"] = json!("europarl_turn:x:1"), "message unit");
            // A mailbox outside S0-9's reserved list.
            let h2 = EnronHalves::from_s09(&s09(&keys(&f.adm), &keys(&f.res[..1]), json!([]))).unwrap();
            assert!(err(EnronReservation::from_rows(listing.iter().cloned().map(Ok), &h2)).contains("not an S0-9 reserved mailbox"));
        }

        #[test]
        fn reserved_render_takes_only_listed_messages_under_the_admission_rules() {
            let f = fixture(&[3, 2]);
            let h = halves_of(&f);
            let res = EnronReservation::from_rows(crate::io::jsonl_rows(&f.scan.join("reserved-metadata.jsonl")).unwrap(), &h).unwrap();
            let out = f.dir.path().join("render");
            let rep = enron_reserved_render(&f.archive, "u", &out, &res, &archive_sha(&f), &[]).unwrap();
            assert_eq!(rep["candidates"], 5);
            assert_eq!(rep["rejection_reasons"]["sender_not_mailbox_owner"], 1);
            assert_eq!(rep["family_buckets"], json!([900, 1000]));
            let c = rows(&out.join("candidates.jsonl"));
            let reserved: BTreeSet<&str> = f.res.iter().map(|o| o.key.as_str()).collect();
            for r in &c {
                assert!(reserved.contains(r["writer_key"].as_str().unwrap()));
                assert_eq!(r["rights"]["evaluation_use"], "diagnostic_only");
                assert_eq!(r["rights"]["hosted_model_use"], "never");
                assert_eq!(r["rights"]["train_use"], false);
                assert_eq!(r["rights"]["cohort_id"], ENRON_DIAGNOSTIC_COHORT);
            }
            let mut texts: Vec<&str> = c.iter().map(|r| r["text"].as_str().unwrap()).collect();
            texts.sort();
            let mut want: Vec<String> = f.reserved_msgs.iter().map(|&(_, s)| excerpt(s)).collect();
            want.sort();
            assert_eq!(texts, want);
            // Same rules: the Train adapter's rules on the same buckets yield the same ids and text
            // (the comparison mode is the Train body without its reserved-bucket stop).
            let tr = f.dir.path().join("train-mode");
            let mut run = Run::start_sampled(&tr, "enron", ENRON_COLLECTION, json!({}), None, Some(ENRON_RESERVED_BUCKETS)).unwrap();
            let x = enron_with(&f.archive, "u", &mut run, &EnronMode::TrainRulesForComparison).unwrap();
            run.finish(x).unwrap();
            let t = rows(&tr.join("candidates.jsonl"));
            let pick = |v: &[Value]| v.iter().map(|r| (r["id"].clone(), r["text"].clone(), r["writer_key"].clone(), r["source_group"].clone())).collect::<Vec<_>>();
            assert_eq!(pick(&c), pick(&t));
            assert_eq!(t[0]["rights"]["evaluation_use"], "train_or_evaluation");
            assert!(t[0]["rights"].get("hosted_model_use").is_none());
        }

        #[test]
        fn reserved_render_refuses_listing_or_archive_drift() {
            let f = fixture(&[2, 1]);
            let h = halves_of(&f);
            let full = EnronReservation::from_rows(crate::io::jsonl_rows(&f.scan.join("reserved-metadata.jsonl")).unwrap(), &h).unwrap();
            // A listed message missing from the archive.
            let mut extra = EnronReservation { units: full.units.clone(), owner_rows: full.owner_rows.clone(), rows: full.rows };
            extra.units.insert("enron_message:000000000000000000000000".into());
            let e = err(enron_reserved_render(&f.archive, "u", &f.dir.path().join("a"), &extra, &archive_sha(&f), &[]));
            assert!(e.contains("not found"), "{e}");
            assert!(!f.dir.path().join("a/report.json").exists());
            // An archive message of a reserved mailbox missing from the listing.
            let mut short = EnronReservation { units: full.units.clone(), owner_rows: full.owner_rows.clone(), rows: full.rows };
            let drop = short.units.iter().next().unwrap().clone();
            short.units.remove(&drop);
            let e = err(enron_reserved_render(&f.archive, "u", &f.dir.path().join("b"), &short, &archive_sha(&f), &[]));
            assert!(e.contains("not in the reservation listing"), "{e}");
            // A mailbox in range that is not listed.
            let mut owners_short = EnronReservation { units: full.units.clone(), owner_rows: full.owner_rows.clone(), rows: full.rows };
            owners_short.owner_rows.remove(&f.res[1].key);
            let e = err(enron_reserved_render(&f.archive, "u", &f.dir.path().join("c"), &owners_short, &archive_sha(&f), &[]));
            assert!(e.contains("not a listed reserved mailbox"), "{e}");
            // A different archive.
            let e = err(enron_reserved_render(&f.archive, "u", &f.dir.path().join("d"), &full, &"0".repeat(64), &[]));
            assert!(e.contains("differs from the reservation scan"), "{e}");
            assert!(!f.dir.path().join("d/report.json").exists());
        }

        fn sums(dir: &Path, names: &[&str]) {
            let mut s = String::new();
            for n in names {
                s.push_str(&format!("{}  ./{n}\n", crate::io::file_sha(&dir.join(n)).unwrap().0));
            }
            std::fs::write(dir.join("SHA256SUMS"), s).unwrap();
        }

        fn sha(p: &Path) -> String {
            crate::io::file_sha(p).unwrap().0
        }

        fn write_rows(p: &Path, rows: &[Value]) {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, rows.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
        }

        const REGISTRY: &str = "admission/prior-registry-pile-v1.json";
        const POOL_FILES: &[&str] = &["ADMISSION.json", "metadata.jsonl", "reserved-metadata.jsonl", "documents.jsonl", "admission/scan-report-enron-scan-v3.json",
                                       "admission/cap-report-enron-c2.json", "admission/dedup-report.json", REGISTRY, ENRON_SUPPLEMENT_REGISTRY, ENRON_SUPPLEMENT_SCAN];
        const POOL: &str = "data/x/pools/enron-v1";

        /// Text placed in the prior files of a fixture pool.
        #[derive(Default)]
        struct Prior {
            /// The admission registry's exact Train-split file.
            train: Vec<String>,
            /// Under the registry's directory-walk and restricted entries: never read.
            walk: Vec<String>,
            restricted: Vec<String>,
            /// The supplementary (RAID) file, and the NIH and EuroParl pools.
            raid: Vec<String>,
            nih: Vec<String>,
            europarl: Vec<String>,
        }

        /// A repository root holding a bound enron-v1 pool, its admission's
        /// other pools and prior files, VALUES-R2.json and a closed STAGE0.json.
        struct Env {
            root: PathBuf,
            pool: PathBuf,
            values: PathBuf,
            stage0: PathBuf,
            also: Vec<PathBuf>,
        }

        fn other_pool(root: &Path, name: &str, coll: &str, texts: &[String]) -> PathBuf {
            let p = root.join("data/x/pools").join(name);
            let docs: Vec<Value> = texts.iter().enumerate().map(|(i, t)| json!({"id": format!("{name}-{i}"), "collection": coll, "text": t, "source_group": format!("{name}:{i}")})).collect();
            write_rows(&p.join("documents.jsonl"), &docs);
            std::fs::write(p.join("ADMISSION.json"), json!({"collection": coll}).to_string()).unwrap();
            sums(&p, &["ADMISSION.json", "documents.jsonl"]);
            p
        }

        /// `cap`: the admission's contributor cap; `extra_docs`: added admitted
        /// enron-v1 rows as (admitted owner index, text).
        fn env(f: &Fixture, name: &str, cap: usize, extra_docs: &[(usize, String)], prior: &Prior) -> Env {
            let root = f.dir.path().join(name);
            let p = root.join(POOL);
            std::fs::create_dir_all(p.join("admission")).unwrap();
            let mut docs = Vec::new();
            for (i, o) in f.adm.iter().enumerate() {
                docs.push(json!({"id": format!("pool-enron-a{i}"), "collection": ENRON_COLLECTION, "writer_key": o.key, "text": excerpt(2000 + i), "source_group": format!("enron_message:a{i}")}));
            }
            for (j, (i, t)) in extra_docs.iter().enumerate() {
                docs.push(json!({"id": format!("pool-enron-x{j}"), "collection": ENRON_COLLECTION, "writer_key": f.adm[*i].key, "text": t, "source_group": format!("enron_message:x{j}")}));
            }
            write_rows(&p.join("documents.jsonl"), &docs);
            std::fs::write(p.join("metadata.jsonl"), "{\"id\": \"m\"}\n").unwrap();
            std::fs::copy(f.scan.join("reserved-metadata.jsonl"), p.join("reserved-metadata.jsonl")).unwrap();
            std::fs::copy(f.scan.join("report.json"), p.join("admission/scan-report-enron-scan-v3.json")).unwrap();
            let n_res = rows(&p.join("reserved-metadata.jsonl")).len();
            std::fs::write(p.join("ADMISSION.json"), json!({"collection": ENRON_COLLECTION, "counts": {"writer_keys": f.adm.len(), "reserved_metadata_rows": n_res}}).to_string()).unwrap();
            std::fs::write(p.join("admission/cap-report-enron-c2.json"), json!({"adapter": "enron", "contributor_cap": {"max_per_contributor": cap, "seed": "slop_ninja_a_pile_cap_v1:fixture", "document_sample_per_mille": null}}).to_string()).unwrap();
            let rowsof = |pre: &str, t: &[String]| -> Vec<Value> { t.iter().enumerate().map(|(i, x)| json!({"id": format!("{pre}-{i}"), "text": x})).collect() };
            write_rows(&root.join("data/x/splits-v5/train/documents.jsonl"), &rowsof("train", &prior.train));
            write_rows(&root.join("datasets/published.jsonl"), &rowsof("pub", &prior.walk));
            write_rows(&root.join("data/x/eval/dev-select/admitted.jsonl"), &rowsof("ds", &prior.restricted));
            write_rows(&root.join("data/x/pools/raid-v1/documents.jsonl"), &rowsof("raid", &prior.raid));
            std::fs::write(p.join(REGISTRY), json!({"schema": crate::protected::REGISTRY_SCHEMA, "registry_version": "pile-v1", "entries": [
                {"label": "published_datasets", "path": "datasets", "names": ["*.jsonl"]},
                {"label": "train_split", "scope": "current_train_split", "path": "data/x/splits-v5/train/documents.jsonl"},
                {"label": "old_dev_select", "path": "data/x/eval/dev-select/admitted.jsonl"},
                {"label": "old_cal", "path": "data/x/eval/cal/admitted.jsonl"}]}).to_string()).unwrap();
            std::fs::write(p.join(ENRON_SUPPLEMENT_REGISTRY), json!({"schema": crate::protected::REGISTRY_SCHEMA, "registry_version": "pile-supplement-raid-v1", "entries": [
                {"label": "raid_pool", "scope": "train_side_pool", "path": "data/x/pools/raid-v1/documents.jsonl"}]}).to_string()).unwrap();
            std::fs::write(p.join(ENRON_SUPPLEMENT_SCAN), json!({"schema": crate::scan::REPORT_SCHEMA, "registry_sha256": sha(&p.join(ENRON_SUPPLEMENT_REGISTRY)),
                "files": [{"label": "raid_pool", "status": "scanned", "sha256": sha(&root.join("data/x/pools/raid-v1/documents.jsonl"))}]}).to_string()).unwrap();
            let also = vec![other_pool(&root, "europarl-v1", EUROPARL_COLLECTION, &prior.europarl), other_pool(&root, "nih-exporter-v1", NIH_COLLECTION, &prior.nih)];
            std::fs::write(p.join("admission/dedup-report.json"), json!({"min_shared_sequences": 1, "options": {"near_window": 12, "near_collections": [ENRON_COLLECTION], "residue_collections": [], "exclude_aliases": []},
                "sources": [{"collection": ENRON_COLLECTION}, {"collection": EUROPARL_COLLECTION}, {"collection": NIH_COLLECTION}]}).to_string()).unwrap();
            let stage0 = root.join("STAGE0.json");
            std::fs::write(&stage0, json!({"schema": STAGE0_SCHEMA, "closed": true}).to_string()).unwrap();
            let e = Env { pool: p, values: root.join("VALUES-R2.json"), stage0, also, root };
            rebind(f, &e);
            e
        }

        /// Refresh the dedup report's per-collection digests from the pools'
        /// documents, then seal the pool as A9 did (`rebind_sealed`).
        fn rebind(f: &Fixture, e: &Env) {
            let dd_p = e.pool.join("admission/dedup-report.json");
            let mut dd: Value = serde_json::from_slice(&std::fs::read(&dd_p).unwrap()).unwrap();
            dd["by_collection_sha256"] = json!({ENRON_COLLECTION: {"sha256": sha(&e.pool.join("documents.jsonl"))},
                                               EUROPARL_COLLECTION: {"sha256": sha(&e.also[0].join("documents.jsonl"))},
                                               NIH_COLLECTION: {"sha256": sha(&e.also[1].join("documents.jsonl"))}});
            std::fs::write(&dd_p, dd.to_string()).unwrap();
            rebind_sealed(f, e);
        }

        /// Seal `e.pool`'s SHA256SUMS and write VALUES binding it (S0-9's
        /// four inputs, repository-relative), as A9 did; tests call it again
        /// after a deliberate edit.
        fn rebind_sealed(f: &Fixture, e: &Env) {
            sums(&e.pool, POOL_FILES);
            let inputs: Vec<Value> = ["ADMISSION.json", "metadata.jsonl", "reserved-metadata.jsonl", "SHA256SUMS"].iter()
                .map(|n| json!({"path": format!("{POOL}/{n}"), "sha256": sha(&e.pool.join(n)), "bytes": 1})).collect();
            let _ = std::fs::remove_file(&e.values);
            std::fs::write(&e.values, json!({"schema": "slop_ninja_a_round2_values_v1", "values": {"S0-9": s09(&keys(&f.adm), &keys(&f.res), json!(inputs))}}).to_string()).unwrap();
        }

        fn run_reserved(f: &Fixture, e: &Env, out: &str) -> Result<Value> {
            enron_reserved(&ReservedInputs { values: &e.values, stage0: &e.stage0, pool: &e.pool, also_pools: &e.also, archive: &f.archive, archive_url: "u",
                                             root: &e.root, threads: 2, out: &f.dir.path().join(out) })
        }

        /// Whitespace words `a..b` of `s`.
        fn words(s: &str, a: usize, b: usize) -> String {
            s.split_whitespace().collect::<Vec<_>>()[a..b].join(" ")
        }

        #[test]
        fn reserved_pipeline_applies_the_admission_parameters() {
            // Owner 0: seven messages for the cap. Owner 1: X and Y share exactly one 50-word
            // run; Z copies an admitted row. Owner 2: A shares exactly near_window (12) words
            // with an admitted row, B one word fewer with another.
            let x50 = words(&excerpt(110), 20, 70);
            let y = format!("Dear team,\n\n{} Regards from the floor.\n\n{x50} Thanks again for all the help on this one.\n\nThanks,\nPat\n\n-----Original Message-----\nFrom: someone\nold quoted text", para(222));
            let f = fixture_with(&[(100..107).map(|s| (s, body(s))).collect::<Vec<_>>(), vec![(110, body(110)), (111, y), (112, body(112))], vec![(120, body(120)), (121, body(121))]]);
            let h = halves_of(&f);
            let diag = f.adm.iter().position(|o| h.diagnostic.contains(&o.key)).unwrap();
            let extra = vec![(0, excerpt(112)), (diag, format!("Alpha beta gamma delta {} omega.", words(&excerpt(120), 30, 42))),
                             (diag, format!("Alpha beta gamma delta {} omega.", words(&excerpt(121), 30, 41)))];
            let e = env(&f, "p", 3, &extra, &Prior::default());
            let rep = run_reserved(&f, &e, "out").unwrap();
            let out = f.dir.path().join("out");
            let rendered = rows(&out.join("rendered/candidates.jsonl"));
            assert_eq!(rep["steps"]["render"]["candidates"], 12);
            // The cap keeps, per mailbox, the lowest SHA-256(seed NUL id) ranks under the
            // admission's recorded seed.
            let seed = "slop_ninja_a_pile_cap_v1:fixture";
            assert_eq!(rep["steps"]["cap"]["seed"], seed);
            assert_eq!(rep["steps"]["cap"]["removed"], 4);
            let mut ranked: Vec<(u64, String)> = rendered.iter().filter(|r| r["writer_key"] == f.res[0].key.as_str())
                .map(|r| (text::h64(format!("{seed}\0{}", r["id"].as_str().unwrap()).as_bytes()), r["id"].as_str().unwrap().to_owned())).collect();
            ranked.sort();
            let want: BTreeSet<String> = ranked.iter().take(3).map(|x| x.1.clone()).collect();
            let capped: BTreeSet<String> = rows(&out.join("capped/candidates.jsonl")).iter().filter(|r| r["writer_key"] == f.res[0].key.as_str()).map(|r| r["id"].as_str().unwrap().to_owned()).collect();
            assert_eq!(capped, want);
            // The window screen: A (12 shared words, with a diagnostic-half row) and Z hit; B does not.
            let by_mid: BTreeMap<usize, String> = rendered.iter().map(|r| (mid(r), r["id"].as_str().unwrap().to_owned())).collect();
            let hit: BTreeSet<String> = rows(&out.join("window-hits.jsonl")).iter().map(|r| r["candidate_id"].as_str().unwrap().to_owned()).collect();
            assert_eq!(hit, BTreeSet::from([by_mid[&112].clone(), by_mid[&120].clone()]));
            assert_eq!(rep["steps"]["window_screen"]["window"], 12);
            // Dedup: Z and A are prior conflicts; one of X and Y is a within-pool duplicate of
            // the other by their single shared 50-word sequence (min_shared 1).
            let exc = rows(&out.join("dedup/exclusions.jsonl"));
            let reasons = |m: usize| exc.iter().find(|x| x["id"] == by_mid[&m].as_str()).map(|x| x["reasons"].clone());
            assert!(reasons(112).unwrap().as_array().unwrap().iter().any(|r| r == "matches_prior_exact_text"), "{exc:?}");
            assert!(reasons(120).is_some() && reasons(121).is_none());
            let xy: Vec<&Value> = exc.iter().filter(|x| x["id"] == by_mid[&110].as_str() || x["id"] == by_mid[&111].as_str()).collect();
            assert_eq!(xy.len(), 1, "{exc:?}");
            assert_eq!(xy[0]["reasons"], json!(["within_pool_shares_folded_50_word_sequence"]));
            assert!(xy[0]["duplicate_of"] == by_mid[&110].as_str() || xy[0]["duplicate_of"] == by_mid[&111].as_str());
            let kept: Vec<Value> = rows(&out.join("dedup/admitted.jsonl"));
            assert_eq!(kept.len(), 5, "owner 0 keeps 3 by the cap; owner 1 one of X and Y; owner 2 keeps B");
            assert_eq!(kept.len() as u64, rep["diagnostic"]["rows"].as_u64().unwrap());
            for r in &kept {
                assert_eq!(r["rights"]["evaluation_use"], "diagnostic_only");
                assert_eq!(r["rights"]["hosted_model_use"], "never");
            }
            assert_eq!(rep["steps"]["dedup"]["min_shared"], 1);
            assert_eq!(rep["calls"]["hosted_model"], 0);
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o700);
                assert_eq!(std::fs::metadata(out.join("dedup/admitted.jsonl")).unwrap().permissions().mode() & 0o777, 0o600);
            }
            // The report names no mailbox key.
            let s = std::fs::read_to_string(out.join("RESERVED-DIAGNOSTIC.json")).unwrap();
            assert!(f.res.iter().chain(&f.adm).all(|o| !s.contains(&o.key)));
        }

        #[test]
        fn reserved_pipeline_screens_the_admission_prior_set() {
            // Owner 0: P (in the registry's exact Train file), Q (under its directory walk) and
            // R (under its restricted entry). Owner 1: W (a 60-word run in the RAID supplement),
            // V (in the NIH pool) and T (clean).
            let f = fixture(&[3, 3]);
            let prior = Prior { train: vec![excerpt(100)], walk: vec![excerpt(101)], restricted: vec![excerpt(102)],
                                raid: vec![format!("Lead in words here {} and a tail.", words(&excerpt(103), 10, 70))], nih: vec![excerpt(104)],
                                europarl: vec![excerpt(3000)] };
            let e = env(&f, "p", 100, &[], &prior);
            let rep = run_reserved(&f, &e, "out").unwrap();
            let out = f.dir.path().join("out");
            let kept: BTreeSet<usize> = rows(&out.join("dedup/admitted.jsonl")).iter().map(mid).collect();
            assert_eq!(kept, BTreeSet::from([101, 102, 105]));
            let exc = rows(&out.join("dedup/exclusions.jsonl"));
            let by_mid: BTreeMap<usize, String> = rows(&out.join("rendered/candidates.jsonl")).iter().map(|r| (mid(r), r["id"].as_str().unwrap().to_owned())).collect();
            let reasons = |m: usize| exc.iter().find(|x| x["id"] == by_mid[&m].as_str()).unwrap()["reasons"].clone();
            assert!(reasons(100).as_array().unwrap().iter().any(|r| r == "matches_prior_exact_text"));
            assert!(reasons(103).as_array().unwrap().iter().any(|r| r == "shares_folded_50_word_sequence_with_prior"));
            assert!(reasons(104).as_array().unwrap().iter().any(|r| r == "matches_prior_exact_text"));
            let p = &rep["prior"];
            assert_eq!(p["kept"], json!([{"label": "train_split", "scope": "current_train_split"}]));
            assert_eq!(p["dropped_restricted"], json!([{"label": "old_dev_select", "rule": "dev-select"}, {"label": "old_cal", "rule": "cal"}]));
            assert_eq!(p["dropped_walks"], json!(["published_datasets"]));
            let colls: Vec<&str> = p["admitted_pools"].as_array().unwrap().iter().map(|x| x["collection"].as_str().unwrap()).collect();
            assert_eq!(colls, [EUROPARL_COLLECTION, NIH_COLLECTION]);
            assert_eq!(p["files"].as_array().unwrap().len(), 4, "the Train file and the three admitted pools' documents");
            assert_eq!(rep["supplement"]["candidates_with_hits"], 1);
            assert_eq!(rep["steps"]["supplement_scan"]["files_scanned"], 1);
            // The report's restricted entries carry a label and a rule, no path; the pools may be
            // given in any order.
            let s = std::fs::read_to_string(out.join("RESERVED-DIAGNOSTIC.json")).unwrap();
            assert!(!s.contains("eval/dev-select") && !s.contains("eval/cal") && !s.contains("datasets/published"));
            let mut e2 = env(&f, "q", 100, &[], &prior);
            e2.also.reverse();
            assert_eq!(run_reserved(&f, &e2, "out2").unwrap()["diagnostic"]["rows"], 3);
        }

        #[test]
        fn reserved_pipeline_refuses_unbound_inputs_before_opening_the_archive() {
            let f = fixture(&[2, 1]);
            let mut n = 0;
            let mut refuse = |e: &Env, want: &str| {
                n += 1;
                let out = format!("r{n}");
                let m = err(run_reserved(&f, e, &out));
                assert!(m.contains(want), "{want}: {m}");
                assert!(!f.dir.path().join(&out).exists(), "{want}: the output was created");
            };
            let fresh = |name: &str| env(&f, name, 100, &[], &Prior::default());
            let e = fresh("p0");
            assert!(run_reserved(&f, &e, "ok").is_ok());
            // Stage 0 still open.
            std::fs::write(&e.stage0, json!({"schema": STAGE0_SCHEMA, "closed": false}).to_string()).unwrap();
            refuse(&e, "Stage 0 open");
            std::fs::write(&e.stage0, json!({"schema": "x", "closed": true}).to_string()).unwrap();
            refuse(&e, "is not STAGE0.json");
            // A pool file changed after SHA256SUMS; SHA256SUMS rewritten to match (S0-9 binds the old one).
            let e = fresh("p1");
            std::fs::write(e.pool.join("admission/cap-report-enron-c2.json"), json!({"adapter": "enron", "contributor_cap": {"max_per_contributor": 1000, "seed": "x"}}).to_string()).unwrap();
            refuse(&e, "cap-report");
            sums(&e.pool, POOL_FILES);
            refuse(&e, "S0-9 binds");
            // Each pre-archive check of the bound records.
            let edit = |name: &str, file: &str, f2: &dyn Fn(&mut Value)| -> Env {
                let e = fresh(name);
                let p = e.pool.join(file);
                let mut v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
                f2(&mut v);
                std::fs::write(&p, v.to_string()).unwrap();
                rebind(&f, &e);
                e
            };
            refuse(&edit("p2", "admission/scan-report-enron-scan-v3.json", &|v| v["family_buckets"] = json!([800, 1000])), "buckets");
            refuse(&edit("p3", "admission/scan-report-enron-scan-v3.json", &|v| v["candidates"] = json!(1)), "text-free");
            refuse(&edit("p4", "ADMISSION.json", &|v| v["counts"]["reserved_metadata_rows"] = json!(5)), "reserved rows differ");
            refuse(&edit("p5", "admission/dedup-report.json", &|v| v["sources"] = json!([{"collection": EUROPARL_COLLECTION}, {"collection": NIH_COLLECTION}])), "is not one of its sources");
            refuse(&edit("p6", ENRON_SUPPLEMENT_SCAN, &|v| v["registry_sha256"] = json!("0")), "not the admission's reverse scan");
            refuse(&edit("p7", ENRON_SUPPLEMENT_SCAN, &|v| v["files"][0]["sha256"] = json!("0")), "the admission's reverse scan read");
            // Admitted documents whose writers are not S0-9's halves.
            let e = fresh("p8");
            let mut d = std::fs::read_to_string(e.pool.join("documents.jsonl")).unwrap();
            d.push_str(&format!("{}\n", json!({"id": "pool-enron-z", "collection": ENRON_COLLECTION, "writer_key": f.res[0].key, "text": excerpt(3001)})));
            std::fs::write(e.pool.join("documents.jsonl"), &d).unwrap();
            rebind(&f, &e);
            refuse(&e, "halves");
            // enron-v1 documents that are not the admission's dedup output.
            let e = fresh("p9");
            let mut d = std::fs::read_to_string(e.pool.join("documents.jsonl")).unwrap();
            d.push_str(&format!("{}\n", json!({"id": "pool-enron-y", "collection": ENRON_COLLECTION, "writer_key": f.adm[0].key, "text": excerpt(3002)})));
            std::fs::write(e.pool.join("documents.jsonl"), &d).unwrap();
            rebind_sealed(&f, &e);
            refuse(&e, "not the admission's dedup output");
            // The other pools: one missing, one given twice, the enron pool itself, a changed one.
            let mut e = fresh("p10");
            let nih = e.also.pop().unwrap();
            refuse(&e, "missing");
            e.also.push(e.also[0].clone());
            refuse(&e, "given twice");
            e.also = vec![e.pool.clone(), nih.clone()];
            refuse(&e, "not another source");
            std::fs::write(nih.join("documents.jsonl"), format!("{}\n", json!({"id": "n", "collection": NIH_COLLECTION, "text": "other"}))).unwrap();
            sums(&nih, &["ADMISSION.json", "documents.jsonl"]);
            let e = Env { also: vec![e.root.join("data/x/pools/europarl-v1"), nih], ..e };
            refuse(&e, "the admission's dedup output for");
            // An unbound admitted pool (its own SHA256SUMS), a pool or prior link to a restricted name.
            let e = fresh("p11");
            std::fs::write(e.also[0].join("documents.jsonl"), "{}\n").unwrap();
            refuse(&e, "documents.jsonl has sha256");
            let mut e = fresh("p12");
            std::fs::create_dir_all(e.root.join("data/x/testing")).unwrap();
            std::fs::rename(&e.also[1], e.root.join("data/x/testing/nih")).unwrap();
            std::os::unix::fs::symlink(e.root.join("data/x/testing/nih"), e.root.join("data/x/pools/nih-link")).unwrap();
            e.also[1] = e.root.join("data/x/pools/nih-link");
            refuse(&e, "resolves to");
            let e = fresh("p13");
            let t = e.root.join("data/x/splits-v5/train/documents.jsonl");
            std::fs::rename(&t, e.root.join("data/x/eval/dev-select/train.jsonl")).unwrap();
            std::os::unix::fs::symlink(e.root.join("data/x/eval/dev-select/train.jsonl"), &t).unwrap();
            refuse(&e, "resolves to");
            // A kept registry file missing; the supplement file changed since the admission's scan.
            let e = fresh("p14");
            std::fs::remove_file(e.root.join("data/x/splits-v5/train/documents.jsonl")).unwrap();
            refuse(&e, "prior registry entry train_split");
            let e = fresh("p15");
            std::fs::write(e.root.join("data/x/pools/raid-v1/documents.jsonl"), "{\"text\": \"changed\"}\n").unwrap();
            refuse(&e, "supplementary prior raid_pool");
            // The pool is not the one S0-9 names under the root; a different archive.
            let e = fresh("p16");
            let e = Env { root: f.dir.path().join("p0"), ..e };
            refuse(&e, "not the one S0-9 names");
            let e = fresh("p17");
            let mut sr: Value = serde_json::from_slice(&std::fs::read(e.pool.join("admission/scan-report-enron-scan-v3.json")).unwrap()).unwrap();
            for i in sr["inputs"].as_array_mut().unwrap() {
                if i["role"] == "enron_maildir_tar_gz" {
                    i["sha256"] = json!("0".repeat(64));
                }
            }
            std::fs::write(e.pool.join("admission/scan-report-enron-scan-v3.json"), sr.to_string()).unwrap();
            rebind(&f, &e);
            refuse(&e, "the reservation scan read");
        }

        #[test]
        fn ood_email_selection_keeps_only_diagnostic_half_writers() {
            let adm = keys(&owners(53, 0, |b| b < 900));
            let res = keys(&owners(1, 0, |b| b >= 900));
            let h = EnronHalves::from_s09(&s09(&adm, &res, json!([]))).unwrap();
            let mut meta = Vec::new();
            for (i, w) in h.train.iter().take(3).chain(h.diagnostic.iter()).enumerate() {
                meta.push(json!({"id": format!("pool-enron-{i}"), "pool": OOD_ENRON_POOL, "writer_key": w}));
            }
            for i in 0..2 {
                meta.push(json!({"id": format!("pool-europarl-{i}"), "pool": "europarl-v1", "writer_key": "europarl_speaker:x"}));
            }
            let ids = |p: &str, m: &[Value]| text::sha_hex(m.iter().filter(|r| r["pool"] == p).map(|r| r["id"].as_str().unwrap()).collect::<Vec<_>>().join("\n").as_bytes());
            let sel = |m: &[Value]| json!({"schema": OOD_SELECTION_SCHEMA, "pools": {
                OOD_ENRON_POOL: {"taken": m.iter().filter(|r| r["pool"] == OOD_ENRON_POOL).count(), "ids_sha256": ids(OOD_ENRON_POOL, m), "documents_sha256": "d0c"},
                "europarl-v1": {"taken": 2, "ids_sha256": ids("europarl-v1", m), "documents_sha256": "e0c"}}});
            let s = sel(&meta);
            let (kept, sum) = ood_email_restriction(&h, &s, meta.iter().cloned().map(Ok), "d0c").unwrap();
            assert_eq!(kept.len(), 3 + 2);
            assert!(kept.iter().filter(|r| r["pool"] == OOD_ENRON_POOL).all(|r| h.diagnostic.iter().any(|d| r["writer_key"] == d.as_str())));
            assert_eq!(sum["email"]["kept"], 3);
            assert_eq!(sum["email"]["dropped_train_half"], 3);
            assert_eq!(sum["pools"]["europarl-v1"]["kept"], 2);
            // Refusals: wrong pool documents, an unknown writer, a missing row, a changed id order, text in meta.
            assert!(err(ood_email_restriction(&h, &s, meta.iter().cloned().map(Ok), "other")).contains("S0-9-bound"));
            let mut m = meta.clone();
            m[0]["writer_key"] = json!(res[0]);
            assert!(err(ood_email_restriction(&h, &sel(&m), m.iter().cloned().map(Ok), "d0c")).contains("neither S0-9 half"));
            assert!(err(ood_email_restriction(&h, &s, meta[1..].iter().cloned().map(Ok), "d0c")).contains("took"));
            let mut m = meta.clone();
            m.swap(0, 1);
            assert!(err(ood_email_restriction(&h, &s, m.iter().cloned().map(Ok), "d0c")).contains("ids_sha256"));
            let mut m = meta.clone();
            m[0]["text"] = json!("x");
            assert!(err(ood_email_restriction(&h, &s, m.iter().cloned().map(Ok), "d0c")).contains("text"));
            let mut m = meta.clone();
            m.push(json!({"id": "q", "pool": "nih-v1", "writer_key": "k"}));
            assert!(err(ood_email_restriction(&h, &s, m.iter().cloned().map(Ok), "d0c")).contains("not in the selection"));
        }

        #[test]
        fn ood_restriction_from_files_binds_the_pool_to_s09() {
            let f = fixture(&[1]);
            let e = env(&f, "p", 100, &[], &Prior::default());
            let h = EnronHalves::from_values_file(&e.values).unwrap().0;
            let meta_p = f.dir.path().join("meta.jsonl");
            let rows_v: Vec<Value> = [&h.train[0], &h.diagnostic[0]].iter().enumerate().map(|(i, w)| json!({"id": format!("e{i}"), "pool": OOD_ENRON_POOL, "writer_key": w})).collect();
            std::fs::write(&meta_p, rows_v.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
            let docs_sha = sha(&e.pool.join("documents.jsonl"));
            let sel_p = f.dir.path().join("selection.json");
            std::fs::write(&sel_p, json!({"schema": OOD_SELECTION_SCHEMA, "pools": {OOD_ENRON_POOL: {"taken": 2, "ids_sha256": text::sha_hex(b"e0\ne1"), "documents_sha256": docs_sha}}}).to_string()).unwrap();
            let out = f.dir.path().join("ood");
            let r = enron_ood_restrict(&e.values, &e.pool, &sel_p, &meta_p, &out).unwrap();
            assert_eq!(r["output"]["rows"], 1);
            let kept = rows(&out.join("meta-restricted.jsonl"));
            assert_eq!(kept[0]["id"], "e1");
            let s = std::fs::read_to_string(out.join("restriction.json")).unwrap();
            assert!(!s.contains(&h.diagnostic[0]) && !s.contains(&h.train[0]));
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(out.join("restriction.json")).unwrap().permissions().mode() & 0o777, 0o600);
            }
            // An unbound pool.
            std::fs::write(e.pool.join("SHA256SUMS"), "").unwrap();
            assert!(err(enron_ood_restrict(&e.values, &e.pool, &sel_p, &meta_p, &f.dir.path().join("ood2"))).contains("S0-9 binds"));
        }
    }
}

//! Santa Barbara Corpus of Spoken American English adapter over the CHAT
//! transcriptions (`SBCSAE_chat.zip`, `SBCSAE/SBCnnn.cha`).
//!
//! * **Rights.** CC BY-ND 3.0 US. Admitted as an ungated evaluation
//!   diagnostic only, never Train, under the classifier-training decision of 2026-10-27; no redistribution.
//! * **Human transcription.** The SBCSAE discourse transcription (Du Bois
//!   et al.) in its TalkBank CHAT conversion.
//! * **Dates.** The LDC release year of the corpus part (part 1: 2000,
//!   part 2: 2003, part 3: 2004, part 4: 2005) as an upper bound; recordings
//!   are from about 1988 to 1996.
//! * **Normalisation** (`NORMALIZATION`). Timing bullets, pauses (`(.)`,
//!   `(..)`, `(1.2)`), overlap marks (`⌈⌉⌊⌋` with indices), `&=` events,
//!   `&{x=` / `&}x=` voice-quality scopes, `[% ...]` comments and other
//!   bracket codes, lengthening colons, glottal marks, `@` word codes and
//!   omitted-sound parentheses inside words are removed and counted;
//!   interruption terminators become `--` and trailing-off terminators
//!   `...`; an intonation-unit line without a terminator gets back the
//!   continuing-contour comma that the CHAT conversion dropped; compounds
//!   lose `_`; `xxx`/`yyy`/`www` break; environment tiers
//!   are skipped.
//! * **Groups.** Family, unit, bucket and split group: one recording
//!   (`sbcsae_recording:SBCnnn`). Speaker codes are per recording; a few
//!   speakers recur across recordings and are not merged (recorded limit).
use crate::{
    adapter::Run,
    record::Doc,
    spoken::{self, Chunk, Removed, Use},
    text::{self, Excerpt},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Read, path::Path};

pub const COLLECTION: &str = "sbcsae_conversation_transcripts";
pub const LICENSE: &str = "CC-BY-ND-3.0-US";
pub const NORMALIZATION: &str = "sbcsae_chat_v1:drop_bullets_pauses_overlap_events_scopes_comments_codes_lengthening_glottal;interruption_to_double_dash;trailing_to_ellipsis;unterminated_intonation_unit_comma_restored;break_xxx_yyy_www;skip_environment";

/// Corpus part release year (upper-bound date) for a recording number.
pub fn part_year(n: u32) -> Option<(u32, u32)> {
    match n {
        1..=14 => Some((1, 2000)),
        15..=30 => Some((2, 2003)),
        31..=46 => Some((3, 2004)),
        47..=60 => Some((4, 2005)),
        _ => None,
    }
}

fn strip_word(tok: &str, rm: &mut Removed) -> String {
    let mut w = tok.to_owned();
    // omitted sounds inside a word: goin(g) -> goin
    while let (Some(a), Some(b)) = (w.find('('), w.find(')')) {
        if b <= a {
            break;
        }
        rm.add("omitted_sound_notation");
        w.replace_range(a..=b, "");
    }
    if let Some(i) = w.find('@') {
        if w[i + 1..].chars().all(|c| c.is_alphanumeric() || c == ':') && i > 0 {
            rm.add("word_code");
            w.truncate(i);
        }
    }
    if w.contains('ʔ') {
        rm.add("glottal_mark");
        w = w.replace('ʔ', "");
    }
    if w.contains(':') && w.chars().any(|c| c.is_alphabetic()) {
        rm.add("lengthening");
        w = w.replace(':', "");
    }
    if w.contains('_') {
        rm.add("compound_underscore");
        w = w.replace('_', " ");
    }
    w
}

/// Normalise one CHAT main-tier utterance (speaker code already removed).
pub fn utterance(u: &str, speaker: &str) -> Chunk {
    let mut rm = Removed::default();
    // timing bullets
    let mut s = String::with_capacity(u.len());
    let mut in_bullet = false;
    for c in u.chars() {
        if c == '\u{15}' {
            if !in_bullet {
                rm.add("timing_bullet");
            }
            in_bullet = !in_bullet;
            continue;
        }
        if !in_bullet {
            s.push(c);
        }
    }
    // bracket codes [% ...], [/], [= ...] etc.
    let mut t = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find('[') {
        t.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) => {
                rm.add(if rest[i..].starts_with("[%") { "transcriber_comment" } else { "chat_bracket_code" });
                t.push(' ');
                rest = &rest[i + j + 1..];
            }
            None => return Chunk::brk(speaker, "unbalanced_square_bracket"),
        }
    }
    t.push_str(rest);
    // overlap marks with optional index digit
    let mut v = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '⌈' | '⌉' | '⌊' | '⌋') {
            rm.add("overlap_mark");
            while chars.peek().is_some_and(|d| d.is_ascii_digit()) {
                chars.next();
            }
            // an overlap mark inside a word joins its halves; at a boundary it leaves a space
            continue;
        }
        v.push(c);
    }
    let mut out = Vec::new();
    for tok in v.split_whitespace() {
        let r = match tok {
            "+/." | "+//." | "+/?" | "+//?" | "+/!" => {
                rm.add("interruption_terminator");
                Some("--".to_owned())
            }
            "+..." | "+..?" | "+..!" => {
                rm.add("trailing_terminator");
                Some("...".to_owned())
            }
            "+," | "+\"" | "+\"/." | "+^" | "+<" | "++" | "+\"." => {
                rm.add("linkage_marker");
                None
            }
            _ if tok.starts_with("&=") => {
                rm.add("paralinguistic_event");
                None
            }
            _ if tok.starts_with("&{") || tok.starts_with("&}") => {
                rm.add("voice_quality_scope");
                None
            }
            _ if tok.starts_with("&-") => {
                rm.add("filler_marker");
                Some(tok[2..].to_owned())
            }
            _ if tok.starts_with("&+") => {
                rm.add("fragment_marker");
                Some(format!("{}-", &tok[2..]))
            }
            _ if tok.starts_with("&~") || tok.starts_with('&') => return Chunk::brk(speaker, "nonword_or_unknown_ampersand_code"),
            _ if tok.starts_with('(') && tok.ends_with(')') && tok[1..tok.len() - 1].chars().all(|c| c == '.' || c == ':' || c.is_ascii_digit()) => {
                rm.add("pause");
                None
            }
            _ if tok.starts_with('0') && tok.len() > 1 && tok[1..].chars().all(char::is_alphabetic) => {
                rm.add("omitted_word_notation");
                None
            }
            _ => {
                let w = strip_word(tok, &mut rm);
                let l = w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
                if (l.len() >= 2 && l.chars().all(|c| c == 'x')) || matches!(l.as_str(), "yyy" | "www" | "yy") {
                    return Chunk::brk(speaker, "unintelligible_or_untranscribed");
                }
                Some(w)
            }
        };
        if let Some(w) = r.filter(|w| !w.is_empty()) {
            if w == "," && out.last().is_none_or(|p: &String| p.ends_with([',', '.', '?', '!', '-'])) {
                continue;
            }
            out.push(w);
        }
    }
    Chunk::speech(speaker, out.join(" "), rm.take())
}

/// One intonation-unit line. The CHAT conversion keeps final (`.`), appeal
/// (`?`) and truncation terminators but drops the discourse transcription's
/// continuing-contour comma, so an IU line without a terminator gets `,`
/// back (`iu_comma_restored`, counted at normalisation as a linkage mark).
pub fn iu(line: &str) -> String {
    let body: String = {
        let mut s = String::new();
        let mut inb = false;
        for c in line.chars() {
            if c == '\u{15}' {
                inb = !inb;
            } else if !inb {
                s.push(c);
            }
        }
        s
    };
    let t = body.trim();
    let last = t.split_whitespace().last().unwrap_or("");
    let terminated = matches!(last, "." | "?" | "!" | "," | "+..." | "+/." | "+//." | "+/?" | "+//?" | "+..?" | "+/!" | "+..!") || last.ends_with(['.', '?', '!']);
    let raw = line.trim();
    if t.is_empty() || terminated {
        raw.to_owned()
    } else {
        match raw.find('\u{15}') {
            Some(i) => format!("{} , {}", raw[..i].trim_end(), &raw[i..]),
            None => format!("{raw} ,"),
        }
    }
}

pub struct Recording {
    pub id: String,
    pub participants: Vec<(String, String)>, // (code, role)
    pub chunks: Vec<Chunk>,
}

pub fn recording(id: &str, cha: &str) -> Recording {
    let mut participants = Vec::new();
    let mut skip = std::collections::HashSet::new();
    let mut utts: Vec<(String, String)> = Vec::new();
    for line in cha.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(p) = line.strip_prefix("@Participants:") {
            for e in p.split(',') {
                let f: Vec<&str> = e.split_whitespace().collect();
                if let (Some(code), Some(role)) = (f.first(), f.last()) {
                    participants.push((code.to_string(), role.to_string()));
                    if *role == "Environment" || *code == "ENV" {
                        skip.insert(code.to_string());
                    }
                }
            }
        } else if let Some(rest) = line.strip_prefix('*') {
            if let Some((code, text)) = rest.split_once(':') {
                utts.push((code.trim().to_owned(), iu(text)));
            }
        } else if line.starts_with('\t') {
            if let Some(last) = utts.last_mut() {
                last.1.push(' ');
                last.1.push_str(&iu(line));
            }
        }
    }
    let chunks = utts
        .into_iter()
        .filter(|(c, _)| !skip.contains(c))
        .map(|(c, t)| utterance(&t, &format!("sbcsae:{id}:{c}")))
        .collect();
    Recording { id: id.to_owned(), participants, chunks }
}

pub fn adapt(zip_path: &Path, run: &mut Run) -> Result<Value> {
    let (zsha, zbytes) = crate::io::file_sha(zip_path)?;
    run.input(json!({"role": "sbcsae_chat_zip", "path": zip_path, "sha256": zsha, "bytes": zbytes}));
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?).context("SBCSAE zip")?;
    let mut names: Vec<String> = z
        .file_names()
        .filter(|n| !n.starts_with("__MACOSX") && n.rsplit('/').next().is_some_and(|b| b.starts_with("SBC") && b.ends_with(".cha")))
        .map(str::to_owned)
        .collect();
    names.sort();
    let method = format!("sbcsae_chat_main_tiers_in_order;{};{};{}", spoken::RENDERING, spoken::RUN_METHOD, NORMALIZATION);
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut removed_total: BTreeMap<&'static str, u64> = BTreeMap::new();
    for name in &names {
        run.units_seen += 1;
        if run.full() {
            break;
        }
        let id = name.rsplit('/').next().unwrap_or("").trim_end_matches(".cha").to_owned();
        let unit = format!("sbcsae_recording:{id}");
        let mut b = Vec::new();
        z.by_name(name)?.read_to_end(&mut b)?;
        let sha = text::sha_hex(&b);
        let num: Option<u32> = id.trim_start_matches("SBC").parse().ok();
        let part = num.and_then(part_year);
        let meta = || json!({"recording": id, "file": name, "file_sha256": sha, "part": part.map(|p| p.0)});
        if !run.sampled_meta(&unit, meta)? {
            continue;
        }
        let Some((part_no, year)) = part else {
            run.reject(&unit, "unknown_corpus_part")?;
            continue;
        };
        let Ok(cha) = String::from_utf8(b) else {
            run.reject(&unit, "not_utf8")?;
            continue;
        };
        let rec = recording(&id, &cha);
        let turns = spoken::turns(rec.chunks);
        for t in &turns {
            if let Some(r) = t.brk {
                *breaks.entry(r).or_default() += 1;
            }
        }
        let wins = spoken::windows(&turns, 0, None);
        if wins.is_empty() {
            run.reject(&unit, "no_turn_run_within_bounds")?;
            continue;
        }
        for w in &wins {
            let wid = format!("{unit}:w{}", w.index);
            if let Some(r) = text::english_screen(&w.text) {
                run.reject(&wid, r)?;
                continue;
            }
            for (k, v) in &w.removed {
                *removed_total.entry(k).or_default() += *v as u64;
            }
            let ex = Excerpt { text: w.text.clone(), start: w.start, end: w.end, words: w.words, complete_unit: w.complete_unit };
            let doc = Doc {
                id: format!("pool-sbcsae-{id}-w{}", w.index),
                collection: COLLECTION,
                domain: "spoken_conversation",
                source_group: unit.clone(),
                writer_key: Some(unit.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "conversation_transcript_turn_run", "span_basis": "merged_turn_index_half_open", "recording": id, "window_index": w.index,
                             "turns": w.end - w.start, "speakers": w.labels.iter().map(|(l, s)| json!({"label": l, "speaker_key": s})).collect::<Vec<_>>(),
                             "normalization": spoken::normalization(w, NORMALIZATION), "rendering": spoken::RENDERING,
                             "short_form_unit": "speaker turns rendered as paragraphs (closest faithful unit for conversation)"}),
                provenance: json!({"recording": id, "corpus_part": part_no, "file": name, "file_sha256": sha, "archive": "SBCSAE_chat.zip", "archive_sha256": zsha,
                                   "transcription": "SBCSAE discourse transcription (human), TalkBank CHAT conversion",
                                   "source_url": "https://www.linguistics.ucsb.edu/research/santa-barbara-corpus-spoken-american-english",
                                   "diagnostic_role": "natural US conversation across regions and classes"}),
                date: Some(format!("{year}-12-31")),
                date_basis: "ldc_corpus_part_release_year_upper_bound_dec_31",
                cutoff_policy: "dated_before_cutoff",
                rights: spoken::rights(LICENSE, "https://creativecommons.org/licenses/by-nd/3.0/us/",
                    format!("Du Bois, John W., et al. Santa Barbara Corpus of Spoken American English, Part {part_no}, {id}; CC BY-ND 3.0 US"),
                    "https://www.linguistics.ucsb.edu/research/santa-barbara-corpus-spoken-american-english",
                    "CC BY-ND 3.0 US (UCSB corpus page; read 2026-10-27); free download without registration",
                    Use::EvalOnly, "spoken_sbcsae_v1", "Evaluation-only diagnostic; no-derivatives licence; never Train or mining."),
                aliases: vec![],
            };
            run.accept(&wid, &doc)?;
        }
    }
    Ok(json!({"files": names.len(), "turn_breaks": breaks, "removed_markup_in_candidates": removed_total, "normalization": NORMALIZATION,
              "limits": ["speaker codes are per recording; recurring speakers across recordings are not merged", "dates are part release years (upper bounds)",
                         "evaluation-only diagnostic; never Train"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chat_utterances_normalise() {
        let c = utterance("the ferry left \u{2308} at noon \u{2309} (..) with three crates of pea:rs [% laughs] , and then +/. \u{15}10_20\u{15}", "s");
        assert_eq!(c.text, "the ferry left at noon with three crates of pears , and then --");
        assert_eq!(c.removed.get("pause"), Some(&1));
        assert_eq!(c.removed.get("overlap_mark"), Some(&2));
        assert_eq!(c.removed.get("lengthening"), Some(&1));
        assert_eq!(c.removed.get("timing_bullet"), Some(&1));
        assert_eq!(c.removed.get("transcriber_comment"), Some(&1));
        assert_eq!(utterance("it was xxx there .", "s").brk, Some("unintelligible_or_untranscribed"));
        let v = utterance("&{l=VOX very unhappy &}l=VOX with ⌊2 our ⌋2 ʔuh rooms +...", "s");
        assert_eq!(v.text, "very unhappy with our uh rooms ...");
        let cha = "@Participants:\tALAN ALAN Speaker, JON JON Speaker, ENV ENV Environment\n*ALAN:\tso we went \u{15}1_2\u{15}\n\t&=in \u{15}2_3\u{15}\n\tdown there . \u{15}3_4\u{15}\n*ENV:\t&=door .\n*JON:\toh God .\n*JON:\tXXXXX .\n";
        let r = recording("SBC999", cha);
        assert_eq!(r.chunks.len(), 3);
        assert_eq!(r.chunks[0].text, "so we went , down there .");
        assert_eq!(r.chunks[2].brk, Some("unintelligible_or_untranscribed"));
        assert_eq!(iu("\tit's a story \u{15}1_2\u{15}"), "it's a story , \u{15}1_2\u{15}");
        assert_eq!(iu("\the s- +... \u{15}1_2\u{15}"), "he s- +... \u{15}1_2\u{15}");
        assert_eq!(r.chunks[0].removed.get("timing_bullet"), Some(&3));
        assert_eq!(r.chunks[1].speaker, "sbcsae:SBC999:JON");
        assert_eq!(part_year(47), Some((4, 2005)));
    }
}

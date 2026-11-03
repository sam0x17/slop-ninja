//! Shared rules for human transcripts of speech (meetings, interviews,
//! conversation): speaker turns, transcription-markup normalisation
//! counters, turn-run excerpts and the rights shapes of the spoken pack.
//!
//! * **Unit.** A transcript is a sequence of speaker turns (consecutive
//!   chunks of one speaker merged). An excerpt is a run of consecutive whole
//!   turns that reaches `TARGET_WORDS`, within `MIN_WORDS..=MAX_WORDS`
//!   whitespace words including the speaker labels; no turn is cut.
//! * **Rendering** (`RENDERING`). One paragraph per turn, prefixed with an
//!   opaque letter label (`A:`, `B:`, ...) assigned in order of first
//!   appearance inside the excerpt; source speaker names never appear.
//! * **Normalisation.** Each source parser removes non-lexical transcription
//!   markup (pauses, breath and laughter codes, overlap brackets, timing,
//!   lengthening and voice-quality marks) and counts every removal by class.
//!   Unintelligible or redacted material, foreign-language or translated
//!   speech, inserted recordings and non-speech channels make the chunk a
//!   *break*: no excerpt crosses it, so placeholders never reach the text.
//!   Disfluencies that are words of the speech (fillers, restarts, word
//!   fragments) stay and their rates are reported per document.
//! * **Residual screen.** A turn that still holds markup characters after
//!   normalisation is a break, so a missed convention cannot become a
//!   "human" cue.
use crate::text::{self, MAX_WORDS, MIN_WORDS};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const TARGET_WORDS: usize = text::TARGET_WORDS;
pub const RENDERING: &str = "speaker_turn_paragraphs_with_opaque_letter_labels_by_first_appearance_v1";
pub const RUN_METHOD: &str = "consecutive_whole_turns_until_220_words_or_break_or_unit_end;150..450_whitespace_words_including_labels;non_overlapping_windows_v1";

/// Characters that never survive normalisation in admitted turns.
pub const RESIDUAL_MARKUP: &[char] = &['<', '>', '[', ']', '{', '}', '\\', '/', '_', '@', '#', '=', '~', '%', '+', '*', '|', '^', '$', '&', '⌈', '⌉', '⌊', '⌋', 'ʔ', '\u{15}'];

/// One transcript chunk in source order, already normalised.
#[derive(Clone, Debug, Default)]
pub struct Chunk {
    /// Opaque speaker key (source speaker code, never a display name).
    pub speaker: String,
    pub text: String,
    /// Break reason: the chunk is not admissible speech and no excerpt
    /// crosses it.
    pub brk: Option<&'static str>,
    /// Markup removed from this chunk, by class.
    pub removed: BTreeMap<&'static str, u32>,
}

impl Chunk {
    pub fn speech(speaker: impl Into<String>, text: impl Into<String>, removed: BTreeMap<&'static str, u32>) -> Self {
        Chunk { speaker: speaker.into(), text: text.into(), brk: None, removed }
    }
    pub fn brk(speaker: impl Into<String>, reason: &'static str) -> Self {
        Chunk { speaker: speaker.into(), text: String::new(), brk: Some(reason), removed: BTreeMap::new() }
    }
}

/// A merged speaker turn.
#[derive(Clone, Debug, Default)]
pub struct Turn {
    pub speaker: String,
    pub text: String,
    pub brk: Option<&'static str>,
    pub removed: BTreeMap<&'static str, u32>,
}

/// Remove-and-count helper used by the source parsers.
#[derive(Default)]
pub struct Removed(pub BTreeMap<&'static str, u32>);
impl Removed {
    pub fn add(&mut self, class: &'static str) {
        *self.0.entry(class).or_default() += 1;
    }
    pub fn take(&mut self) -> BTreeMap<&'static str, u32> {
        std::mem::take(&mut self.0)
    }
}

/// Collapse whitespace and drop spaces left before punctuation by removed
/// markup ("word ." -> "word."); returns the text and whether spacing was
/// changed beyond whitespace collapsing.
pub fn tidy(s: &str) -> (String, bool) {
    let n = text::normalized(s);
    let mut out = String::with_capacity(n.len());
    let mut changed = false;
    let chars: Vec<char> = n.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' && i + 1 < chars.len() && matches!(chars[i + 1], ',' | '.' | '?' | '!' | ';' | ':') && i > 0 && chars[i - 1].is_alphanumeric() {
            // keep " ..." and " --" style marks; only single punctuation tokens attach
            let next2 = chars.get(i + 2).copied();
            if next2.is_none() || next2 == Some(' ') {
                changed = true;
                continue;
            }
        }
        out.push(c);
    }
    (out, changed)
}

/// Turn-level screen after normalisation.
pub fn turn_reason(t: &str) -> Option<&'static str> {
    turn_reason_with(t, RESIDUAL_MARKUP)
}

pub fn turn_reason_with(t: &str, residual: &[char]) -> Option<&'static str> {
    if let Some(r) = text::damaged(t) {
        return Some(r);
    }
    if text::has_url(t) {
        return Some("url");
    }
    if t.chars().any(|c| residual.contains(&c)) {
        return Some("residual_transcription_markup");
    }
    if t.chars().any(|c| c.is_alphabetic()) && text::ascii_letter_ratio(t) < 0.9 {
        return Some("non_ascii_letters");
    }
    if text::words(t) > MAX_WORDS {
        return Some("turn_over_450_words");
    }
    None
}

/// Merge consecutive speech chunks of one speaker into turns. A break chunk
/// is its own turn. Chunks whose text is empty after normalisation (only
/// non-lexical markup) are dropped; their removal counts move to the next
/// kept turn so per-document rates still see them.
pub fn turns(chunks: Vec<Chunk>) -> Vec<Turn> {
    turns_with(chunks, RESIDUAL_MARKUP)
}

/// `turns` with a source-specific residual markup set.
pub fn turns_with(chunks: Vec<Chunk>, residual: &[char]) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    let mut pending: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut last_was_speech = false;
    for c in chunks {
        if let Some(r) = c.brk {
            out.push(Turn { speaker: c.speaker, text: String::new(), brk: Some(r), removed: BTreeMap::new() });
            last_was_speech = false;
            continue;
        }
        let mut removed = c.removed;
        let t = c.text.trim();
        if !t.chars().any(|ch| ch.is_alphanumeric()) {
            for (k, v) in removed {
                *pending.entry(k).or_default() += v;
            }
            continue;
        }
        for (k, v) in std::mem::take(&mut pending) {
            *removed.entry(k).or_default() += v;
        }
        match out.last_mut() {
            Some(prev) if last_was_speech && prev.speaker == c.speaker => {
                prev.text.push(' ');
                prev.text.push_str(t);
                for (k, v) in removed {
                    *prev.removed.entry(k).or_default() += v;
                }
            }
            _ => out.push(Turn { speaker: c.speaker, text: t.to_owned(), brk: None, removed }),
        }
        last_was_speech = true;
    }
    for t in out.iter_mut().filter(|t| t.brk.is_none()) {
        let (s, _) = tidy(&t.text);
        t.text = s;
        t.brk = turn_reason_with(&t.text, residual);
    }
    out
}

/// One rendered excerpt.
#[derive(Clone, Debug)]
pub struct Window {
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub words: usize,
    /// label -> opaque speaker key
    pub labels: Vec<(String, String)>,
    pub removed: BTreeMap<&'static str, u32>,
    pub complete_unit: bool,
}

fn label(i: usize) -> String {
    let mut s = String::new();
    let mut n = i;
    loop {
        s.insert(0, (b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

/// Non-overlapping turn-run windows, starting at turn `first`, at most
/// `cap` of them.
pub fn windows(turns: &[Turn], first: usize, cap: Option<usize>) -> Vec<Window> {
    let mut out = Vec::new();
    let n = turns.len();
    let mut i = first.min(n);
    while i < n && cap.is_none_or(|c| out.len() < c) {
        if turns[i].brk.is_some() {
            i += 1;
            continue;
        }
        let mut w = 0;
        let mut j = i;
        while j < n && turns[j].brk.is_none() {
            w += text::words(&turns[j].text) + 1;
            j += 1;
            if w >= TARGET_WORDS {
                break;
            }
        }
        let run_ended = j == n || turns[j].brk.is_some();
        if (MIN_WORDS..=MAX_WORDS).contains(&w) && (w >= TARGET_WORDS || run_ended) {
            let mut labels: Vec<(String, String)> = Vec::new();
            let mut paras = Vec::new();
            let mut removed: BTreeMap<&'static str, u32> = BTreeMap::new();
            for t in &turns[i..j] {
                let l = match labels.iter().find(|(_, s)| *s == t.speaker) {
                    Some((l, _)) => l.clone(),
                    None => {
                        let l = label(labels.len());
                        labels.push((l.clone(), t.speaker.clone()));
                        l
                    }
                };
                paras.push(format!("{l}: {}", t.text));
                for (k, v) in &t.removed {
                    *removed.entry(k).or_default() += v;
                }
            }
            let text = paras.join("\n\n");
            out.push(Window { index: out.len(), start: i, end: j, words: text::words(&text), text, labels, removed, complete_unit: i == 0 && j == n });
            i = j;
            continue;
        }
        i = if w > MAX_WORDS { i + 1 } else { j };
    }
    out
}

/// Spoken-language features kept in the text, per 100 words, so they can be
/// matched or reported (they are speech, not markup).
pub fn speech_rates(t: &str) -> Value {
    const FILLERS: &[&str] = &["uh", "um", "uhm", "er", "erm", "ah", "eh", "hm", "hmm", "mm", "mhm", "mm-hmm", "uh-huh", "huh"];
    let toks: Vec<String> = t.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-').to_lowercase()).collect();
    let n = toks.len().max(1) as f64;
    let fillers = toks.iter().filter(|w| FILLERS.contains(&w.as_str())).count();
    let fragments = t.split_whitespace().filter(|w| {
        let w = w.trim_end_matches([',', '.', '?', '!']);
        w.len() > 1 && w.ends_with('-') && !w.ends_with("--")
    }).count();
    let dashes = t.split_whitespace().filter(|w| *w == "-" || *w == "--").count();
    json!({"fillers_per_100_words": fillers as f64 * 100.0 / n, "word_fragments_per_100_words": fragments as f64 * 100.0 / n,
           "standalone_dashes_per_100_words": dashes as f64 * 100.0 / n,
           "kept_speech_features": "fillers, word fragments and restart dashes are words or marks of the speech and are kept"})
}

/// The normalisation record of one window.
pub fn normalization(w: &Window, rule: &str) -> Value {
    let total: u32 = w.removed.values().sum();
    json!({"rule": rule, "removed_markup_by_class": w.removed, "removed_markup_total": total,
           "removed_markup_per_100_words": total as f64 * 100.0 / w.words.max(1) as f64,
           "speech_rates": speech_rates(&w.text),
           "labels": RENDERING})
}

/// Basis recorded for spoken sources admitted outside the default licence set
/// under the classifier-training decision of 2026-10-27 (lawful access,
/// no redistribution, removable cohort).
pub const CLASSIFIER_TRAINING_BASIS: &str = crate::record::CLASSIFIER_TRAINING_BASIS;

/// Which splits a source may enter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Use {
    /// Train, Train-mining and evaluation.
    Any,
    /// Train and Train-mining only.
    TrainOnly,
    /// Ungated evaluation diagnostics only; never Train or mining.
    EvalOnly,
}

impl Use {
    pub fn splits(self) -> Value {
        match self {
            Use::Any => json!(["train", "train_mining", "evaluation"]),
            Use::TrainOnly => json!(["train", "train_mining"]),
            Use::EvalOnly => json!(["evaluation_diagnostic_ungated"]),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Use::Any => "train_train_mining_and_evaluation",
            Use::TrainOnly => "train_and_train_mining_only",
            Use::EvalOnly => "evaluation_diagnostic_only_never_train",
        }
    }
}

/// Rights object for a spoken source. `default_licence` sources (CC BY)
/// use the established shape; others carry the classifier-training basis.
#[allow(clippy::too_many_arguments)]
pub fn rights(license: &str, license_url: &str, attribution: String, evidence_url: &str, terms: &str, use_: Use, cohort: &str, notes: &str) -> Value {
    let mut r = crate::record::rights(license, attribution, evidence_url, notes);
    r["license_url"] = json!(license_url);
    r["redistribute_text"] = json!(false);
    r["terms_recorded"] = json!(terms);
    r["allowed_splits"] = use_.splits();
    r["use_restriction"] = json!(use_.name());
    r["source_cohort"] = json!(cohort);
    r["removable_by_source_cohort"] = json!(true);
    r["opaque_speaker_keys"] = json!(true);
    if crate::record::admitted_license(license) {
        r["admission_decision"] = json!(format!("spoken_pack_2026-10-27: {license} source admitted for {}; human transcripts only, text kept in ignored storage", use_.name()));
    } else {
        r["admission_basis"] = json!(CLASSIFIER_TRAINING_BASIS);
        r["admission_decision"] = json!(format!("classifier-training use, user decision 2026-10-27: {license} source admitted for {}; lawful direct access without login; no redistribution", use_.name()));
        r["commercial_training"] = json!(use_ != Use::EvalOnly);
        r["model_release"] = json!(use_ != Use::EvalOnly);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sp(s: &str, t: &str) -> Chunk {
        Chunk::speech(s, t, BTreeMap::new())
    }
    fn words(n: usize, w: &str) -> String {
        vec![w; n].join(" ") + "."
    }
    #[test]
    fn turns_merge_drop_empty_and_screen_residue() {
        let mut r = BTreeMap::new();
        r.insert("pause", 2);
        let t = turns(vec![
            sp("s1", "we went to the market ,"),
            sp("s1", "and then home ."),
            Chunk { speaker: "s2".into(), text: " ".into(), brk: None, removed: r },
            sp("s2", "okay sure."),
            Chunk::brk("s2", "unintelligible"),
            sp("s1", "it was [fine]."),
        ]);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0].text, "we went to the market, and then home.");
        assert_eq!(t[1].removed.get("pause"), Some(&2));
        assert_eq!(t[2].brk, Some("unintelligible"));
        assert_eq!(t[3].brk, Some("residual_transcription_markup"));
    }
    #[test]
    fn windows_never_cross_breaks_and_label_by_first_appearance() {
        let mut ch = Vec::new();
        for i in 0..6 {
            ch.push(sp(if i % 2 == 0 { "x9" } else { "y3" }, &words(59, "talk")));
        }
        ch.push(Chunk::brk("x9", "redaction"));
        for i in 0..3 {
            ch.push(sp(if i % 2 == 0 { "y3" } else { "z1" }, &words(49, "word")));
        }
        let t = turns(ch);
        let w = windows(&t, 0, None);
        // 60 words + label per turn: four turns reach 240, then two turns (120) end at the break and are too short.
        assert_eq!(w.len(), 2, "{:?}", w.iter().map(|w| (w.start, w.end, w.words)).collect::<Vec<_>>());
        assert_eq!((w[0].start, w[0].end, w[0].words), (0, 4, 240));
        assert!(w[0].text.starts_with("A: talk") && w[0].text.contains("\n\nB: talk"));
        assert_eq!(w[0].labels, vec![("A".to_owned(), "x9".to_owned()), ("B".to_owned(), "y3".to_owned())]);
        // after the break: three turns of 50 words = 150, run ends at unit end
        assert_eq!((w[1].start, w[1].end, w[1].words), (7, 10, 150));
        assert!(w[1].text.starts_with("A: word"));
        assert_eq!(w[1].labels[0].1, "y3");
        assert_eq!(windows(&t, 0, Some(1)).len(), 1);
        assert_eq!(label(0), "A");
        assert_eq!(label(26), "AA");
    }
    #[test]
    fn overlong_turns_break_and_rates_count_speech_features() {
        let t = turns(vec![sp("a", &words(460, "go")), sp("b", &words(200, "so")), sp("a", &words(30, "um"))]);
        assert_eq!(t[0].brk, Some("turn_over_450_words"));
        let w = windows(&t, 0, None);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].start, 1);
        let r = speech_rates("A: um I th- think - uh so");
        assert!(r["fillers_per_100_words"].as_f64().unwrap() > 20.0);
        assert!(r["word_fragments_per_100_words"].as_f64().unwrap() > 0.0);
        assert_eq!(tidy("so , we went .").0, "so, we went.");
        assert_eq!(tidy("so ... we went").0, "so ... we went");
    }
    #[test]
    fn rights_mark_non_default_licences_with_the_basis() {
        let r = rights("CC-BY-ND-3.0-US", "https://creativecommons.org/licenses/by-nd/3.0/us/", "a".into(), "u", "t", Use::EvalOnly, "c", "n");
        assert_eq!(r["admission_basis"], CLASSIFIER_TRAINING_BASIS);
        assert_eq!(r["commercial_training"], false);
        assert_eq!(r["redistribute_text"], false);
        assert!(crate::record::admitted_rights(&r));
        let r = rights("CC-BY-4.0", "https://creativecommons.org/licenses/by/4.0/", "a".into(), "u", "t", Use::Any, "c", "n");
        assert!(r.get("admission_basis").is_none());
        assert!(crate::record::admitted_rights(&r));
        let mut bad = rights("CC-BY-ND-3.0-US", "x", "a".into(), "u", "t", Use::EvalOnly, "c", "n");
        bad["redistribute_text"] = json!(true);
        assert!(!crate::record::admitted_rights(&bad));
    }
}

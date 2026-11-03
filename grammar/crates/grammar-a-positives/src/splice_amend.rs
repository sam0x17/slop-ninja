//! spans-v1.1 amendments to the spliced records (private PROTOCOL-v1.1,
//! amendments A3-A5; tracked summary `docs/a-spans-protocol.md`).
//!
//! - **Meta screen v2.** Assistant prefaces, edit and persona labels, prompt
//!   and skill echoes, word-count notes, and scripts the parent never uses. A
//!   pattern hit counts unless the same matched text also occurs in the parent;
//!   a script counts unless the parent has a character of that script.
//! - **Typography.** In a keyed half of the documents, generated characters the
//!   parent never uses (curly quotes and primes, hyphen and dash variants,
//!   no-break and other space variants, the ellipsis) are mapped to the
//!   parent's conventions, and straight quotes in a parent that only uses curly
//!   ones become curly. Whitespace maps to whitespace and every other character
//!   to non-whitespace, so words, span boundaries and the share are unchanged.
//!   The other half keeps the raw generated text.
//! - **Copied words.** A word-level LCS alignment of each edit's output against
//!   the human text it replaced marks the output words copied verbatim (after a
//!   typographic fold). They stay AI words under the share definition; the mask
//!   lets span metrics be reported with and without them.
use crate::splice::{self, Part, py_space, py_word_ranges};
use crate::text;
use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

pub const META_SCREEN: &str = "slop_ninja_spliced_meta_screen_v2";
pub const TYPOGRAPHY_POLICY: &str = "slop_ninja_span_typography_v1";
pub const COPY_RULE: &str = "slop_ninja_span_copied_words_v1";
pub const AMENDMENT: &str = "spans-v1.1";

/// Patterns over the lowercased, quote-folded output (case-insensitive).
pub const META_V2: [(&str, &str); 9] = [
    (
        "lead_label",
        r"^\s*(?:(?:the|my|your|a)\s+)?(?:new\s+)?(?:(?:rewritten|revised|edited|condensed|expanded|polished|paraphrased|simplified|formali[sz]ed|humani[sz]ed|final|improved)\s+)?(?:passage|edit|sentence|rewrite|revision|answer|output|response|result|paraphrase|text|clause|version|paragraph|observation)(?:\s*\d+)?(?:\s+(?:edited|rewritten|revised|to edit|reads|is|follows|below|here|text|version|passage|sentence))*\s*(?:\([^)\n]{0,30}\))?\s*:",
    ),
    (
        "lead_assistant",
        r"^\s*(?:okay|ok|alright|sure|certainly|absolutely|of course)[,!.]?\s+(?:here(?:'s| is| are)\b|this is (?:the|your|my|a)\b|below is\b|i(?:'ll| will) (?:rewrite|rephrase|revise|edit|condense|expand|polish|paraphrase|make|try|give)\b|let me (?:rewrite|rephrase|revise|edit|condense|expand|polish|paraphrase|try|give it)\b)",
    ),
    ("heres_your", r"\bhere'?s (?:your|my) |\bhere (?:is|are) (?:your|my) "),
    (
        "meta_noun",
        r"\b(?:rewritten|revised|edited|condensed|expanded|polished|paraphrased|humani[sz]ed|simplified|formali[sz]ed)\s+(?:passage|sentence|version|text|clause|paragraph)\b",
    ),
    ("following_passage", r"\bthe following (?:passage|text|sentence|paragraph)\b|\bin this (?:rewrite|revision|rewritten)\b|\bto rephrase the\b"),
    (
        "skill_echo",
        r"\bllms?\b|\blanguage models?\b|\bwriting tells?\b|\b(?:llm|ai) tells?\b|\bai[- ]generated\b|\bslop\b|\bas per (?:the )?skill\b|\bskill guidelines?\b|\bhard bans?\b|\bnegation[- ]contrast\b|\bthroat[- ]clearing\b|\bmic[- ]drop\b|\bmeta[- ]narration\b|\btricolon\b",
    ),
    ("note_paren", r"\((?:note|edited|edit|changes|revised|rewritten)\b[^)]{0,200}\)"),
    ("word_count", r"\(\s*\d+\s*words?\b|\b\d+\s+words?\s*[.)]?\s*$|\bword count\b"),
    ("voice_label", r"\bvoice\s*(?:\(\d+[^)]*\))?\s*:"),
];

fn meta_regexes() -> &'static [(&'static str, Regex)] {
    static R: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    R.get_or_init(|| META_V2.iter().map(|(k, p)| (*k, Regex::new(&format!("(?i){p}")).expect("meta pattern"))).collect())
}

/// Lowercased with curly quotes folded (the v1 screens' normal form).
pub fn fold(s: &str) -> String {
    s.replace(['’', '‘'], "'").replace(['“', '”'], "\"").to_lowercase()
}

/// Script of a letter outside Latin and Common, by Unicode block.
pub fn script_of(c: char) -> Option<&'static str> {
    Some(match c as u32 {
        0x0370..=0x03FF | 0x1F00..=0x1FFF => "greek",
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => "cyrillic",
        0x0530..=0x058F => "armenian",
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => "hebrew",
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFC => "arabic",
        0x0900..=0x0DFF => "indic",
        0x0E00..=0x0E7F => "thai",
        0x10A0..=0x10FF => "georgian",
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => "hangul",
        0x3040..=0x309F => "hiragana",
        0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => "katakana",
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF => "han",
        0x3000..=0x303F | 0xFF00..=0xFF65 => "cjk_punctuation",
        _ => return None,
    })
}

/// Meta screen v2 reasons for one generated output against its parent.
pub fn meta_screen(output: &str, parent: &str) -> Vec<String> {
    let (o, p) = (fold(output), fold(parent));
    let mut why = Vec::new();
    for (k, re) in meta_regexes() {
        if re.find_iter(&o).any(|m| !p.contains(m.as_str().trim())) {
            why.push(format!("meta_v2:{k}"));
        }
    }
    let have: HashSet<&str> = parent.chars().filter_map(script_of).collect();
    let mut miss: Vec<&str> = output.chars().filter_map(script_of).filter(|s| !have.contains(s)).collect();
    miss.sort_unstable();
    miss.dedup();
    why.extend(miss.into_iter().map(|s| format!("script_absent_from_parent:{s}")));
    why
}

// ---------------------------------------------------------------- typography

const SINGLE: [char; 4] = ['\u{2018}', '\u{2019}', '\u{201B}', '\u{2032}'];
const DOUBLE: [char; 4] = ['\u{201C}', '\u{201D}', '\u{201F}', '\u{2033}'];
const HYPHENS: [char; 2] = ['\u{2010}', '\u{2011}'];
const DASHES: [char; 4] = ['\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}'];
const SPACES: [char; 14] = [
    '\u{00A0}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}',
];
const NO_BREAK: [char; 3] = ['\u{00A0}', '\u{2007}', '\u{202F}'];

/// Keyed typography stratum of one spliced document (50/50 by SHA-256 key).
pub fn typography_stratum(id: &str) -> &'static str {
    if splice::ukey("typography", id) % 2 == 0 { "normalized" } else { "raw" }
}

fn opening(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(p) => py_space(p) || matches!(p, '(' | '[' | '{' | '-' | '/' | '\u{2013}' | '\u{2014}'),
    }
}

fn cp(s: &str) -> String {
    s.chars().map(|c| format!("U+{:04X}", c as u32)).collect::<Vec<_>>().join(" ")
}

/// A generated character that marks the span typographically: it belongs to a
/// normalizable class (or is a straight quote in a curly-only parent) and the
/// parent never uses it.
pub fn is_marker(c: char, parent_chars: &HashSet<char>) -> bool {
    if parent_chars.contains(&c) {
        return false;
    }
    SINGLE.contains(&c)
        || DOUBLE.contains(&c)
        || HYPHENS.contains(&c)
        || DASHES.contains(&c)
        || SPACES.contains(&c)
        || c == '\u{2026}'
        || (c == '\'' && parent_chars.contains(&'\u{2019}'))
        || (c == '"' && (parent_chars.contains(&'\u{201C}') || parent_chars.contains(&'\u{201D}')))
}

/// Marker characters in `s` absent from the parent, by code point.
pub fn markers(s: &str, parent_chars: &HashSet<char>) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for c in s.chars().filter(|&c| is_marker(c, parent_chars)) {
        *m.entry(format!("U+{:04X}", c as u32)).or_default() += 1;
    }
    m
}

/// Map one generated span to the parent's typographic conventions. Returns the
/// new text and the substitutions made ("U+2019>U+0027" -> count).
pub fn normalize_typography(s: &str, parent: &str) -> (String, BTreeMap<String, usize>) {
    let pc: HashSet<char> = parent.chars().collect();
    let has = |cs: &[char]| cs.iter().any(|c| pc.contains(c));
    let (parent_dots, parent_double_hyphen) = (parent.contains("..."), parent.contains("--"));
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut subs: BTreeMap<String, usize> = BTreeMap::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let (prev, next) = (i.checked_sub(1).map(|j| chars[j]), chars.get(i + 1).copied());
        let mut take = 1;
        let rep: Option<String> = if c == '.' && chars[i..].starts_with(&['.', '.', '.']) && !parent_dots && pc.contains(&'\u{2026}') {
            take = 3;
            Some("\u{2026}".into())
        } else if pc.contains(&c) {
            None
        } else if SINGLE.contains(&c) {
            (!has(&SINGLE)).then(|| "'".into())
        } else if DOUBLE.contains(&c) {
            (!has(&DOUBLE)).then(|| "\"".into())
        } else if HYPHENS.contains(&c) {
            Some(HYPHENS.iter().find(|h| pc.contains(h)).map_or_else(|| "-".to_owned(), |h| h.to_string()))
        } else if DASHES.contains(&c) {
            let range = prev.is_some_and(|p| p.is_ascii_digit()) && next.is_some_and(|n| n.is_ascii_digit());
            Some(
                if range {
                    if pc.contains(&'\u{2013}') { "\u{2013}" } else { "-" }
                } else if pc.contains(&'\u{2014}') {
                    "\u{2014}"
                } else if pc.contains(&'\u{2013}') {
                    "\u{2013}"
                } else if parent_double_hyphen {
                    "--"
                } else {
                    "-"
                }
                .into(),
            )
        } else if SPACES.contains(&c) {
            Some(if NO_BREAK.contains(&c) && pc.contains(&'\u{00A0}') { "\u{00A0}" } else { " " }.into())
        } else if c == '\u{2026}' {
            Some("...".into())
        } else if c == '\'' && pc.contains(&'\u{2019}') {
            let open = opening(prev) && next.is_some_and(|n| !py_space(n)) && pc.contains(&'\u{2018}');
            Some(if open { "\u{2018}" } else { "\u{2019}" }.into())
        } else if c == '"' && (pc.contains(&'\u{201C}') || pc.contains(&'\u{201D}')) {
            let (lo, ro) = (pc.contains(&'\u{201C}'), pc.contains(&'\u{201D}'));
            let open = opening(prev) && next.is_some_and(|n| !py_space(n));
            Some(if (open && lo) || !ro { "\u{201C}" } else { "\u{201D}" }.into())
        } else {
            None
        };
        let src: String = chars[i..i + take].iter().collect();
        match rep {
            Some(r) if r != src => {
                debug_assert!(!r.is_empty() && src.chars().all(py_space) == r.chars().all(py_space));
                *subs.entry(format!("{}>{}", cp(&src), cp(&r))).or_default() += 1;
                out.push_str(&r);
            }
            _ => out.push_str(&src),
        }
        i += take;
    }
    (out, subs)
}

// ---------------------------------------------------------------- copied words

fn fold_word(w: &str) -> String {
    let mut o = String::with_capacity(w.len());
    for c in w.chars() {
        match c {
            c if SINGLE.contains(&c) => o.push('\''),
            c if DOUBLE.contains(&c) => o.push('"'),
            c if HYPHENS.contains(&c) || DASHES.contains(&c) => o.push('-'),
            '\u{2026}' => o.push_str("..."),
            c => o.push(c),
        }
    }
    o
}

/// LCS alignment runs of `after` words copied from `before` words:
/// (after_word_start, length, before_word_start), contiguous in both.
/// Exact word match after `fold_word`; the earliest alignment (forward trace
/// over the suffix table, a match taken whenever the words are equal).
pub fn copied_runs(before: &str, after: &str) -> Vec<(usize, usize, usize)> {
    let b: Vec<String> = py_word_ranges(before).iter().map(|&(s, e)| fold_word(&before[s..e])).collect();
    let a: Vec<String> = py_word_ranges(after).iter().map(|&(s, e)| fold_word(&after[s..e])).collect();
    let (n, m) = (b.len(), a.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let w = m + 1;
    let mut t = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if b[i] == a[j] { t[(i + 1) * w + j + 1] + 1 } else { t[(i + 1) * w + j].max(t[i * w + j + 1]) };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    while i < n && j < m {
        if b[i] == a[j] {
            match runs.last_mut() {
                Some(r) if r.0 + r.1 == j && r.2 + r.1 == i => r.1 += 1,
                _ => runs.push((j, 1, i)),
            }
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    runs
}

// ---------------------------------------------------------------- records

fn span_of(v: &Value) -> Result<(usize, usize)> {
    let a = v.as_array().context("span")?;
    ensure!(a.len() == 2, "span pair");
    Ok((a[0].as_u64().context("span start")? as usize, a[1].as_u64().context("span end")? as usize))
}

fn merge(mut v: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(l) if s <= l.1 => l.1 = l.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Amend one accepted spliced record (already screened): typography for the
/// normalized stratum, copied-word masks for every edit, and the recomputed
/// offsets and digests. The raw splice is first rebuilt from the parent and
/// must reproduce the record exactly; words, AI words and the share must be
/// unchanged by the normalization.
pub fn amend_record(rec: &Value, parent: &str) -> Result<Value> {
    let id = rec["id"].as_str().context("id")?;
    let textv = rec["text"].as_str().context("text")?;
    let edits = rec["edits"].as_array().context("edits")?;
    let mut parts = Vec::new();
    let mut raw: Vec<&str> = Vec::new();
    for e in edits {
        let (fs, fe) = span_of(&e["final_span_bytes"])?;
        let r = textv.get(fs..fe).context("final span")?;
        ensure!(text::sha_hex(r.as_bytes()) == e["after_sha256"].as_str().unwrap_or(""), "{id}: after digest");
        parts.push(Part { parent_span: span_of(&e["parent_span_bytes"])?, insert_sep: e["insert_separator"].as_str() });
        raw.push(r);
    }
    let asm_raw = splice::assemble_parts(parent, &parts, &raw)?;
    ensure!(asm_raw.text == textv, "{id}: raw splice does not reproduce the record");
    let stratum = typography_stratum(id);
    let pc: HashSet<char> = parent.chars().collect();
    let mut reps: Vec<String> = Vec::new();
    let mut subs: Vec<BTreeMap<String, usize>> = Vec::new();
    for r in &raw {
        let (t, s) = if stratum == "normalized" { normalize_typography(r, parent) } else { (r.to_string(), BTreeMap::new()) };
        ensure!(splice::py_words(&t) == splice::py_words(r), "{id}: typography changed the word count");
        reps.push(t);
        subs.push(s);
    }
    let rr: Vec<&str> = reps.iter().map(String::as_str).collect();
    let asm = splice::assemble_parts(parent, &parts, &rr)?;
    ensure!(asm.ai_words == asm_raw.ai_words && asm.total_words == asm_raw.total_words, "{id}: word accounting changed");
    let ci = |b: usize| splice::char_index(&asm.text, b);
    let mut out = rec.clone();
    let mut new_edits = Vec::new();
    let (mut doc_subs, mut doc_markers_raw, mut doc_markers) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (mut changed, mut copied_total, mut copied_spans) = (0usize, 0usize, Vec::new());
    for (k, e) in edits.iter().enumerate() {
        let mut e = e.clone();
        let (fs, fe) = asm.edit_spans[k];
        let (ps, pe) = parts[k].parent_span;
        let before = &parent[ps..pe];
        let after = rr[k];
        let mr = markers(raw[k], &pc);
        let mf = markers(after, &pc);
        for (c, n) in &mr {
            *doc_markers_raw.entry(c.clone()).or_insert(0usize) += n;
        }
        for (c, n) in &mf {
            *doc_markers.entry(c.clone()).or_insert(0usize) += n;
        }
        for (c, n) in &subs[k] {
            *doc_subs.entry(c.clone()).or_insert(0usize) += n;
        }
        let is_changed = after != raw[k];
        changed += usize::from(is_changed);
        e["final_span_bytes"] = json!([fs, fe]);
        e["final_span_chars"] = json!([ci(fs), ci(fe)]);
        e["after_sha256"] = json!(text::sha_hex(after.as_bytes()));
        e["typography"] = json!({"stratum": stratum, "changed": is_changed, "after_raw_sha256": text::sha_hex(raw[k].as_bytes()),
            "substitutions": subs[k], "markers_raw": mr, "markers": mf});
        let words = py_word_ranges(after);
        let runs = copied_runs(before, after);
        let n_copied: usize = runs.iter().map(|r| r.1).sum();
        let spans: Vec<(usize, usize)> = runs.iter().map(|&(a, l, _)| (ci(fs + words[a].0), ci(fs + words[a + l - 1].1))).collect();
        copied_total += n_copied;
        copied_spans.extend(spans.iter().copied());
        e["copied"] = json!({"rule": COPY_RULE, "words": n_copied, "words_after": words.len(),
            "longest_run": runs.iter().map(|r| r.1).max().unwrap_or(0),
            "runs": runs.iter().map(|&(a, l, b)| json!([a, l, b])).collect::<Vec<_>>(), "char_spans": spans});
        new_edits.push(e);
    }
    let hist = Value::Array(new_edits);
    out["edit_history_sha256"] = json!(text::sha_hex(&serde_json::to_vec(&hist)?));
    out["edits"] = hist;
    out["text"] = json!(asm.text);
    out["text_sha256"] = json!(text::sha_hex(asm.text.as_bytes()));
    out["ai_byte_spans"] = json!(asm.ai_byte_spans);
    out["ai_char_spans"] = json!(asm.ai_byte_spans.iter().map(|&(a, b)| (ci(a), ci(b))).collect::<Vec<_>>());
    out["typography"] = json!({"policy": TYPOGRAPHY_POLICY, "stratum": stratum, "edits_changed": changed, "substitutions": doc_subs,
        "markers_raw": doc_markers_raw, "markers": doc_markers,
        "rule": "keyed half normalized: generated quote, dash, hyphen, space and ellipsis characters absent from the parent take the parent's form; straight quotes in a curly-only parent become curly; whitespace stays whitespace"});
    out["copy_rule"] = json!(COPY_RULE);
    out["ai_copied_words"] = json!(copied_total);
    out["ai_copied_char_spans"] = json!(merge(copied_spans));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_screen_v2_hits_and_parent_exemptions() {
        let p = "The committee met on Tuesday. Members voted twice.";
        for (out, want) in [
            ("Passage edited: The committee met on Tuesday.", "meta_v2:lead_label"),
            ("Edit: Members voted twice.", "meta_v2:lead_label"),
            ("New Sentence: They agreed.", "meta_v2:lead_label"),
            ("Okay, here's your rewritten passage: they met.", "meta_v2:lead_assistant"),
            ("Okay, here's your rewritten passage: they met.", "meta_v2:heres_your"),
            ("Okay, here's your rewritten passage: they met.", "meta_v2:meta_noun"),
            ("The following passage has been edited for LLM writing tells.", "meta_v2:following_passage"),
            ("The following passage has been edited for LLM writing tells.", "meta_v2:skill_echo"),
            ("They voted twice (edited to remove slop phrases as per skill guidelines).", "meta_v2:note_paren"),
            ("They voted twice. (37 words)", "meta_v2:word_count"),
            ("They voted twice, 46 words.", "meta_v2:word_count"),
            ("Elevator voice: they voted twice.", "meta_v2:voice_label"),
            ("They voted twice in the 会议 hall.", "script_absent_from_parent:han"),
            ("They voted twice, а потом left.", "script_absent_from_parent:cyrillic"),
        ] {
            assert!(meta_screen(out, p).iter().any(|r| r == want), "{out} -> {want}: {:?}", meta_screen(out, p));
        }
        for (out, want) in [
            ("The revised passage reads: they met.", "meta_v2:lead_label"),
            ("Passage to edit: they met.", "meta_v2:lead_label"),
            ("Rewritten version (formal): they met.", "meta_v2:lead_label"),
            ("Sentence 2: they met.", "meta_v2:lead_label"),
            ("Alright, let me rephrase that. They met.", "meta_v2:lead_assistant"),
        ] {
            assert!(meta_screen(out, p).iter().any(|r| r == want), "{out} -> {want}");
        }
        // clean outputs, ordinary sentence openings and parent-borne patterns pass
        assert!(meta_screen("The members met on Tuesday and voted twice.", p).is_empty());
        assert!(meta_screen("The response seemed petulant: they left.", p).is_empty());
        assert!(meta_screen("Text messages: none arrived.", p).is_empty());
        assert!(meta_screen("Alright, I'll toss some flyers your way.", p).is_empty());
        let q = "Result: the motion failed. A language model was cited. Слово.";
        assert!(meta_screen("Result: the motion failed again, the language model said. Слово.", q).is_empty());
        assert!(meta_screen("It ran to 500 words of text and stopped.", p).is_empty());
    }

    #[test]
    fn typography_maps_to_parent_conventions_without_moving_words() {
        let ascii = "It's a \"plain\" text - with hyphens.";
        let (t, s) = normalize_typography("It’s “odd”—really\u{2011}so 10\u{202F}km, 1990–2000…", ascii);
        assert_eq!(t, "It's \"odd\"-really-so 10 km, 1990-2000...");
        assert_eq!(s["U+2019>U+0027"], 1);
        assert_eq!(s["U+2014>U+002D"], 1);
        assert_eq!(s["U+2026>U+002E U+002E U+002E"], 1);
        // parent with an em dash, a no-break space and a double hyphen keeps its forms
        let (t, _) = normalize_typography("a\u{2013}b 10\u{202F}km", "x\u{2014}y 5\u{00A0}m -- z");
        assert_eq!(t, "a\u{2014}b 10\u{00A0}km");
        let (t, _) = normalize_typography("a\u{2014}b", "x -- y");
        assert_eq!(t, "a--b");
        // curly-only parent: straight quotes become curly
        let curly = "“Yes,” she said. It’s ‘fine’.";
        let (t, _) = normalize_typography("\"Don't,\" he said... 'ok'", curly);
        assert_eq!(t, "“Don’t,” he said... ‘ok’");
        let (t, _) = normalize_typography("wait...", "Then… nothing.");
        assert_eq!(t, "wait…");
        // characters the parent already uses are kept
        let (t, s) = normalize_typography("It’s—fine", "Its’—x");
        assert_eq!((t.as_str(), s.len()), ("It’s—fine", 0));
        for (a, p) in [("x\u{00A0}y\u{2009}z “q” ‘r’ s\u{2011}t", ascii), ("\"a\" 'b' c", curly)] {
            let (t, _) = normalize_typography(a, p);
            assert_eq!(py_word_ranges(&t).len(), py_word_ranges(a).len());
        }
        let pc: HashSet<char> = ascii.chars().collect();
        assert_eq!(markers("It’s 10\u{202F}km—so", &pc).len(), 3);
        assert!(markers("It's 10 km - so", &pc).is_empty());
    }

    #[test]
    fn copied_runs_are_lcs_word_runs() {
        let before = "The old mill stood by the river for two hundred years.";
        let after = "For two hundred years the old mill stood beside the river.";
        // case- and punctuation-exact words: "old mill stood" + "the" (4) beats "two hundred" (2)
        assert_eq!(copied_runs(before, after), vec![(5, 3, 1), (9, 1, 5)]);
        let same = copied_runs("a b c d", "a b c d");
        assert_eq!(same, vec![(0, 4, 0)]);
        assert_eq!(copied_runs("It’s here", "It's here"), vec![(0, 2, 0)]);
        assert!(copied_runs("", "x y").is_empty());
    }

    #[test]
    fn amend_record_rebuilds_and_normalizes_with_unchanged_words() {
        let parent = "One two three. Four five six. Seven eight nine.";
        let d = splice::Doc::new(parent);
        let (s, e) = d.span(1, 2);
        let rep = "Quatre’s “five” six.";
        let text = format!("{}{}{}", &parent[..s], rep, &parent[e..]);
        let fs = s;
        let fe = s + rep.len();
        let mut rec = json!({"id": "", "text": text, "edits": [{"edit_index": 0, "final_span_bytes": [fs, fe], "parent_span_bytes": [s, e],
            "insert_separator": null, "after_sha256": text::sha_hex(rep.as_bytes())}], "ai_byte_spans": [[fs, fe]]});
        for id in ["a", "b", "c", "d"] {
            rec["id"] = json!(id);
            let out = amend_record(&rec, parent).unwrap();
            let t = out["text"].as_str().unwrap();
            let sp = span_of(&out["ai_byte_spans"][0]).unwrap();
            let words = |x: &str| py_word_ranges(x).len();
            assert_eq!(words(t), words(&text));
            assert_eq!(&t[..sp.0], &parent[..s]);
            assert_eq!(&t[sp.1..], &parent[e..]);
            let normalized = typography_stratum(id) == "normalized";
            assert_eq!(&t[sp.0..sp.1], if normalized { "Quatre's \"five\" six." } else { rep });
            assert_eq!(out["edits"][0]["copied"]["words"], 1);
            assert_eq!(out["edits"][0]["typography"]["changed"], normalized);
            assert_eq!(out["ai_copied_words"], 1);
        }
        // a record whose text the edit history does not reproduce is refused
        rec["text"] = json!(format!("{text} "));
        assert!(amend_record(&rec, parent).is_err());
    }
}

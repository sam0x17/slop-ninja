//! Word counts, hashes, 50-word sequences and excerpt boundaries.
//!
//! Word counts are whitespace tokens, the measure used by every admitted A
//! human row (`150..=450`). The whitespace-normalised hash and the 50-word
//! whitespace sequences reproduce the established overlap method
//! (`exact_and_whitespace_normalized_text_50_word_sequences_and_scratch_prompts_v1`).
//! A case- and punctuation-folded variant is added on top; it only ever
//! excludes more text.
use sha2::{Digest, Sha256};

// The prose screen, word counts, whitespace normalisation, `h64` and the
// admitted-document bounds live in `grammar_a_form::text`, the one source the
// form path shares; they are re-exported here under their existing paths.
pub use grammar_a_form::text::{
    GAP, MAX_WORDS, MIN_WORDS, Para, Screen, ascii_letter_ratio, damaged, ends_sentence, h64, has_url, long_quotation, normalized, prose_reason,
    squash, words,
};

/// Paragraph runs stop at the first paragraph boundary at or after this many
/// words, matching the "at least 220 words" book rule; a complete unit that
/// is shorter but still within the bound is admitted whole.
pub const TARGET_WORDS: usize = 220;
/// Sequence length for near-duplicate detection.
pub const SHINGLE: usize = 50;
/// Strings shorter than this are not indexed as protected prose.
pub const MIN_INDEXED_WORDS: usize = 20;

pub fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn normalized_sha(s: &str) -> String {
    sha_hex(normalized(s).as_bytes())
}

/// Lowercase alphanumeric tokens; every other character separates tokens.
pub fn folded_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn folded(s: &str) -> String {
    folded_tokens(s).join(" ")
}

pub fn folded_sha(s: &str) -> String {
    sha_hex(format!("folded\0{}", folded(s)).as_bytes())
}

/// Hashes of every 50-token whitespace window, joined by one space. These
/// are the same windows the stage-3 overlap audit hashed.
pub fn shingles_ws(s: &str) -> Vec<u64> {
    let w = s.split_whitespace().collect::<Vec<_>>();
    w.windows(SHINGLE).map(|x| h64(x.join(" ").as_bytes())).collect()
}

/// Hashes of every 50-token folded window (domain separated).
pub fn shingles_folded(s: &str) -> Vec<u64> {
    let w = folded_tokens(s);
    w.windows(SHINGLE)
        .map(|x| h64(format!("folded\0{}", x.join(" ")).as_bytes()))
        .collect()
}

/// Hashes of every `n`-token folded window (domain separated by `n`), sorted
/// and unique. Used for short-window near-duplicate screens.
pub fn shingles_folded_n(s: &str, n: usize) -> Vec<u64> {
    let w = folded_tokens(s);
    let mut v: Vec<u64> = w.windows(n.max(1)).map(|x| h64(format!("folded{n}\0{}", x.join(" ")).as_bytes())).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Rendering residue the conservative MediaWiki renderer does not mark:
/// the post-render screen applied to the Wikipedia pool v1, now in Rust.
/// Pipe characters, an empty `( )` after a non-word character, a literal
/// numeric citation marker `[n]` (one to three digits) and unbalanced square
/// brackets.
pub fn markup_residue(s: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if s.contains('|') {
        out.push("pipe_markup_residue");
    }
    let b = s.as_bytes();
    let word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let empty_parens = s.char_indices().any(|(i, c)| {
        if c != '(' || (i > 0 && word(b[i - 1])) {
            return false;
        }
        s[i + 1..].trim_start().starts_with(')')
    });
    if empty_parens {
        out.push("empty_parentheses_rendering_hole");
    }
    let numeric_marker = s.match_indices('[').any(|(i, _)| {
        let rest = &b[i + 1..];
        let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
        (1..=3).contains(&digits) && rest.get(digits) == Some(&b']')
    });
    if numeric_marker {
        out.push("literal_numeric_citation_marker");
    }
    if s.matches('[').count() != s.matches(']').count() {
        out.push("unbalanced_square_brackets");
    }
    out
}

/// u64 keys for the three whole-text hashes.
pub fn text_keys(s: &str) -> [u64; 3] {
    [
        h64(s.as_bytes()),
        h64(normalized(s).as_bytes()),
        h64(format!("folded\0{}", folded(s)).as_bytes()),
    ]
}

/// A run of consecutive whole prose paragraphs.
#[derive(Clone, Debug, PartialEq)]
pub struct Excerpt {
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub words: usize,
    pub complete_unit: bool,
}

pub const PARAGRAPH_RUN_METHOD: &str = "first_consecutive_whole_prose_paragraphs_until_220_words_or_unit_end;150..450_whitespace_words;paragraphs_joined_by_blank_line_v1";

/// Take consecutive whole prose paragraphs from the first run that can reach
/// the bound. Stop at the first paragraph boundary at or after
/// `TARGET_WORDS`; a run that ends the unit may be shorter if it has at
/// least `MIN_WORDS`. Never truncates a paragraph.
pub fn paragraph_run(paras: &[Para]) -> Option<Excerpt> {
    let all_prose = paras.iter().all(|p| p.prose);
    let mut i = 0;
    while i < paras.len() {
        if !paras[i].prose {
            i += 1;
            continue;
        }
        let mut n = 0;
        let mut j = i;
        while j < paras.len() && paras[j].prose {
            n += words(&paras[j].text);
            j += 1;
            if n >= TARGET_WORDS {
                break;
            }
        }
        let run_ended = j == paras.len() || !paras[j].prose;
        if (MIN_WORDS..=MAX_WORDS).contains(&n) && (n >= TARGET_WORDS || run_ended) {
            let text = paras[i..j]
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            return Some(Excerpt {
                words: words(&text),
                text,
                start: i,
                end: j,
                complete_unit: all_prose && i == 0 && j == paras.len(),
            });
        }
        // Overshoot: try the next paragraph of the same run. Too short: the
        // run ended, so continue after it.
        i = if n > MAX_WORDS { i + 1 } else { j };
    }
    None
}

/// Exact Project Gutenberg book excerpt, identical to
/// `body_percent20_60_next_sentence_220words_end_sentence_max600_v1`.
/// Returns absolute byte offsets into the whole file text.
pub fn pg_excerpt(text: &str, fraction: usize) -> Result<(usize, usize), &'static str> {
    let (b, e) = pg_body(text)?;
    let t = &text[b..e];
    let offsets = t
        .char_indices()
        .filter_map(|(i, c)| {
            (!c.is_whitespace() && (i == 0 || t[..i].ends_with(char::is_whitespace))).then_some(i)
        })
        .collect::<Vec<_>>();
    if offsets.len() <= 1500 {
        return Err("short_source_work");
    }
    let desired = offsets[offsets.len() * fraction / 100];
    let boundary = t[desired..]
        .char_indices()
        .find_map(|(i, c)| {
            let end = desired + i + c.len_utf8();
            (matches!(c, '.' | '?' | '!')
                && end < t.len()
                && t[end..].starts_with(char::is_whitespace))
            .then_some(end)
        })
        .ok_or("no_sentence_start_boundary")?;
    let start = boundary + (t[boundary..].len() - t[boundary..].trim_start().len());
    let mut count = 0usize;
    let mut end = None;
    for (i, c) in t[start..].char_indices() {
        if !c.is_whitespace() && (i == 0 || t[start..start + i].ends_with(char::is_whitespace)) {
            count += 1;
        }
        let p = start + i + c.len_utf8();
        if count >= 220
            && matches!(c, '.' | '?' | '!')
            && (p == t.len() || t[p..].starts_with(char::is_whitespace))
        {
            end = Some(p);
            break;
        }
    }
    let end = end.ok_or("no_sentence_end_boundary")?;
    if t[start..end].split_whitespace().count() > 600 {
        return Err("excerpt_exceeds_600_words");
    }
    Ok((b + start, b + end))
}

pub const PG_EXCERPT_METHOD: &str = "body_percent20_60_next_sentence_220words_end_sentence_max600_v1";

/// Successor book rule: the v1 start and end rules, but an excerpt that
/// overlaps quotation material restarts after it (the Wikinews and Stack
/// Exchange rule that quotations break runs), within the window up to the
/// next 10% position. An excerpt with no quotation is byte-identical to v1.
pub const PG_EXCERPT_METHOD_V2: &str = "body_percent_next_sentence_220words_end_sentence_max600_quotation_break_v2";
/// Quoted spans and indented blocks of at least this many words are
/// quotation material.
pub const QUOTATION_WORDS: usize = 8;

/// One stretch of quotation material inside a PG body (byte offsets
/// relative to the body).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuoteRegion {
    pub start: usize,
    pub end: usize,
    /// Whole indented paragraph (block quotation, verse, inset letter).
    pub block: bool,
}

/// Paragraphs of a plain-text body: byte ranges separated by blank lines.
fn plain_paragraphs(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut cur: Option<(usize, usize)> = None;
    let mut pos = 0usize;
    for line in t.split_inclusive('\n') {
        let (ls, le) = (pos, pos + line.trim_end_matches(['\n', '\r']).len());
        pos += line.len();
        if line.trim().is_empty() {
            if let Some(p) = cur.take() {
                out.push(p);
            }
        } else {
            cur = Some(match cur {
                Some((s, _)) => (s, le),
                None => (ls, le),
            });
        }
    }
    if let Some(p) = cur {
        out.push(p);
    }
    out
}

fn indented(line: &str) -> bool {
    line.starts_with('\t') || line.starts_with("  ")
}

/// Quoted spans of at least `QUOTATION_WORDS` words inside one paragraph.
/// Pairs: straight double quotes (alternating), “…”, «…» and ‘…’ (a ’
/// between two letters is an apostrophe, not a closer). An opener without a
/// closer runs to the paragraph end, the convention for quotations that
/// continue into the next paragraph.
fn inline_quotes(t: &str, ps: usize, pe: usize, out: &mut Vec<QuoteRegion>) {
    let p = &t[ps..pe];
    let chars: Vec<(usize, char)> = p.char_indices().collect();
    let alpha = |k: usize| k < chars.len() && chars[k].1.is_alphabetic();
    let mut k = 0usize;
    while k < chars.len() {
        let (oi, c) = chars[k];
        let close: Option<char> = match c {
            '"' => Some('"'),
            '“' => Some('”'),
            '«' => Some('»'),
            '‘' => Some('’'),
            _ => None,
        };
        let Some(close) = close else {
            k += 1;
            continue;
        };
        let mut m = k + 1;
        let mut found = None;
        while m < chars.len() {
            let ch = chars[m].1;
            let apostrophe = ch == '’' && m > 0 && chars[m - 1].1.is_alphabetic() && alpha(m + 1);
            if ch == close && !apostrophe {
                found = Some(m);
                break;
            }
            m += 1;
        }
        let (inner_end, region_end, next) = match found {
            Some(m) => (chars[m].0, chars[m].0 + close.len_utf8(), m + 1),
            None => (p.len(), p.len(), chars.len()),
        };
        let inner = &p[oi + c.len_utf8()..inner_end];
        if words(inner) >= QUOTATION_WORDS {
            out.push(QuoteRegion { start: ps + oi, end: ps + region_end, block: false });
        }
        k = next;
    }
}

/// Quotation material in a PG plain-text body. Indented paragraphs whose
/// every line is indented (block quotations, verse, inset letters) count in
/// every stratum, unless the book indents most multi-line paragraphs that
/// way (a typesetting convention, not quotation). With `inline`, quoted
/// spans of eight or more words count too; the adapter sets it for
/// non-fiction strata, where they are mostly other writers' words, and not
/// for fiction, where they are mostly the author's own dialogue.
pub fn pg_quotation_regions(t: &str, inline: bool) -> Vec<QuoteRegion> {
    let paras = plain_paragraphs(t);
    let full_indent = |&(s, e): &(usize, usize)| t[s..e].lines().filter(|l| !l.trim().is_empty()).all(indented);
    let multi: Vec<&(usize, usize)> = paras.iter().filter(|(s, e)| t[*s..*e].lines().count() >= 3).collect();
    let indented_multi = multi.iter().filter(|p| full_indent(p)).count();
    let blocks_enabled = multi.is_empty() || indented_multi * 2 <= multi.len();
    let mut out = Vec::new();
    for p in &paras {
        if blocks_enabled && full_indent(p) && words(&t[p.0..p.1]) >= QUOTATION_WORDS {
            out.push(QuoteRegion { start: p.0, end: p.1, block: true });
        } else if inline {
            inline_quotes(t, p.0, p.1, &mut out);
        }
    }
    out.sort_by_key(|r| (r.start, r.end));
    out
}

/// First sentence start at or after `pos` under the v1 rule: just after the
/// first `.`, `?` or `!` that is followed by whitespace.
fn sentence_start_after(t: &str, pos: usize) -> Option<usize> {
    let boundary = t[pos..].char_indices().find_map(|(i, c)| {
        let end = pos + i + c.len_utf8();
        (matches!(c, '.' | '?' | '!') && end < t.len() && t[end..].starts_with(char::is_whitespace)).then_some(end)
    })?;
    Some(boundary + (t[boundary..].len() - t[boundary..].trim_start().len()))
}

/// End of the v1 excerpt that starts at `start`: the first sentence end at
/// or after 220 words.
fn excerpt_end(t: &str, start: usize) -> Option<usize> {
    let mut count = 0usize;
    for (i, c) in t[start..].char_indices() {
        if !c.is_whitespace() && (i == 0 || t[start..start + i].ends_with(char::is_whitespace)) {
            count += 1;
        }
        let p = start + i + c.len_utf8();
        if count >= 220 && matches!(c, '.' | '?' | '!') && (p == t.len() || t[p..].starts_with(char::is_whitespace)) {
            return Some(p);
        }
    }
    None
}

/// Outcome of the v2 excerpt rule: absolute byte offsets plus how many
/// quotation restarts it took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PgExcerpt {
    pub start: usize,
    pub end: usize,
    pub quotation_restarts: usize,
}

/// `PG_EXCERPT_METHOD_V2`. Returns absolute byte offsets into the file text.
pub fn pg_excerpt_v2(text: &str, fraction: usize, inline_quotes: bool) -> Result<PgExcerpt, &'static str> {
    let (b, e) = pg_body(text)?;
    let t = &text[b..e];
    let offsets = t
        .char_indices()
        .filter_map(|(i, c)| (!c.is_whitespace() && (i == 0 || t[..i].ends_with(char::is_whitespace))).then_some(i))
        .collect::<Vec<_>>();
    if offsets.len() <= 1500 {
        return Err("short_source_work");
    }
    let desired = offsets[offsets.len() * fraction / 100];
    let limit = offsets[(offsets.len() * (fraction + 10) / 100).min(offsets.len() - 1)];
    let regions = pg_quotation_regions(t, inline_quotes);
    let mut start = sentence_start_after(t, desired).ok_or("no_sentence_start_boundary")?;
    let mut restarts = 0usize;
    loop {
        if restarts > 0 && start >= limit {
            return Err("quotation_no_clean_excerpt_in_window");
        }
        let end = excerpt_end(t, start).ok_or(if restarts > 0 { "quotation_no_clean_excerpt_in_window" } else { "no_sentence_end_boundary" })?;
        match regions.iter().find(|r| r.start < end && r.end > start) {
            None => {
                if t[start..end].split_whitespace().count() > 600 {
                    return Err("excerpt_exceeds_600_words");
                }
                return Ok(PgExcerpt { start: b + start, end: b + end, quotation_restarts: restarts });
            }
            Some(r) => {
                restarts += 1;
                // After a block, the next paragraph starts a sentence; after
                // an inline quotation, resume at the next sentence start.
                let next = if r.block {
                    let rest = &t[r.end..];
                    (!rest.trim().is_empty()).then(|| r.end + (rest.len() - rest.trim_start().len()))
                } else {
                    sentence_start_after(t, r.end)
                };
                start = next.ok_or("quotation_no_clean_excerpt_in_window")?;
            }
        }
    }
}

/// Function words for the English screen of book excerpts.
const ENGLISH_STOPWORDS: &[&str] = &[
    "the", "of", "and", "to", "a", "in", "that", "is", "was", "he", "for", "it", "with", "as", "his", "on", "be", "at",
    "by", "i", "this", "had", "not", "are", "but", "from", "or", "have", "an", "they", "which", "you", "were", "her",
    "she", "there", "would", "their", "we", "him", "been", "has", "when", "who", "will", "no", "if", "so", "what",
    "its", "into", "than", "them", "could", "my", "me", "our", "your", "these", "those", "then", "do", "did", "any",
];
/// Minimum share of Latin-script letters among all letters in a book excerpt.
pub const MIN_LATIN_LETTER_SHARE: f64 = 0.9;
/// Minimum share of English function words among folded tokens.
pub const MIN_ENGLISH_STOPWORD_SHARE: f64 = 0.2;

/// Share of Latin-script letters (ASCII, Latin-1 Supplement, Latin
/// Extended-A/B and Latin Extended Additional) among all letters.
pub fn latin_letter_share(s: &str) -> f64 {
    let (mut l, mut all) = (0usize, 0usize);
    for c in s.chars().filter(|c| c.is_alphabetic()) {
        all += 1;
        let u = c as u32;
        if c.is_ascii_alphabetic() || (0xC0..=0x24F).contains(&u) || (0x1E00..=0x1EFF).contains(&u) {
            l += 1;
        }
    }
    if all == 0 { 0.0 } else { l as f64 / all as f64 }
}

/// Share of folded tokens that are common English function words.
pub fn english_stopword_share(s: &str) -> f64 {
    let toks = folded_tokens(s);
    if toks.is_empty() {
        return 0.0;
    }
    toks.iter().filter(|t| ENGLISH_STOPWORDS.contains(&t.as_str())).count() as f64 / toks.len() as f64
}

/// Text-level language screen for book excerpts, on top of the RDF `en` tag.
pub fn english_screen(s: &str) -> Option<&'static str> {
    if latin_letter_share(s) < MIN_LATIN_LETTER_SHARE {
        return Some("latin_letter_share_below_0_90");
    }
    if english_stopword_share(s) < MIN_ENGLISH_STOPWORD_SHARE {
        return Some("english_stopword_share_below_0_20");
    }
    None
}

pub fn pg_body(text: &str) -> Result<(usize, usize), &'static str> {
    let marker = text.find("*** START OF").ok_or("no_pg_start_marker")?;
    let start = marker + text[marker..].find('\n').ok_or("no_pg_marker_line")? + 1;
    let end = text[start..].find("*** END OF").ok_or("no_pg_end_marker")? + start;
    if start >= end {
        return Err("empty_pg_body");
    }
    Ok((start, end))
}

/// `Wharton, Edith (Edith Newbold)` -> `edith_wharton`.
pub fn author_key(catalog_name: &str) -> String {
    let base = catalog_name.split('(').next().unwrap_or(catalog_name);
    let mut parts = base.splitn(2, ',');
    let last = parts.next().unwrap_or("").trim();
    let first = parts.next().unwrap_or("").trim();
    let joined = if first.is_empty() {
        last.to_owned()
    } else {
        format!("{first} {last}")
    };
    underscore_key(&joined)
}

pub fn underscore_key(s: &str) -> String {
    folded_tokens(s).join("_")
}

/// `The Custom of the Country: A Novel` -> `custom_of_the_country`.
pub fn title_key(title: &str) -> String {
    let main = title.split([':', ';', '(']).next().unwrap_or(title);
    let mut w = folded_tokens(&main.replace(['\'', '’'], ""));
    if w.first().is_some_and(|x| ["a", "an", "the"].contains(&x.as_str())) {
        w.remove(0);
    }
    w.join("_")
}

/// Catalog-inventory style aliases: `title:<alnum>` with and without a
/// leading article, for the full and main title.
pub fn title_aliases(title: &str) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for raw in [title, title.split([':', ';', '(']).next().unwrap_or(title)] {
        let w = folded_tokens(&raw.replace(['\'', '’'], ""));
        if w.is_empty() {
            continue;
        }
        out.insert(format!("title:{}", w.concat()));
        if ["a", "an", "the"].contains(&w[0].as_str()) && w.len() > 1 {
            out.insert(format!("title:{}", w[1..].concat()));
        }
    }
    out.into_iter().collect()
}

pub const TRANSCRIPTION_ARTIFACT_METHOD: &str = "slop_ninja_transcription_artifact_screen_v1";

/// Folded tokens that are almost always OCR confusions in English prose.
const OCR_CONFUSION_TOKENS: &[&str] = &[
    "tbe", "tlie", "thc", "aud", "wbich", "wbo", "tbat", "tben", "bave", "tbis", "witb", "wbat", "wben", "tbey", "tbeir",
    "liave", "bis", "tbere", "ibe", "arc", "bcen", "frorn", "rnore", "sbe", "liis", "tliat", "tliis",
];
/// Bracketed editorial or transcription notes.
const EDITORIAL_MARKS: &[&str] = &[
    "[sic", "[illegible", "[unclear", "[?]", "[...]", "[…]", "[page", "[blank", "[missing", "[torn", "[inaudible",
    "[crosstalk", "[laughter", "[transcriber", "[editor", "[ed.", "[footnote", "[note",
];

/// OCR and transcription artefacts in rendered text, as named markers.
/// Strong markers (every one except `digit_inside_word`) make text an easy
/// "human" signal and reject the candidate; `digit_inside_word` is only
/// recorded. The text itself is never changed.
pub fn transcription_artifacts(s: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if s.contains('ſ') {
        out.push("long_s");
    }
    if s.chars().any(|c| ('\u{FB00}'..='\u{FB06}').contains(&c)) {
        out.push("typographic_ligature");
    }
    if s.contains('\u{AD}') {
        out.push("soft_hyphen");
    }
    if s.contains('¬') {
        out.push("not_sign_line_break");
    }
    // "exam- ple": a lowercase letter, a hyphen, one space, a lowercase letter
    // (an unrepaired line-end hyphenation once lines are joined).
    let cs: Vec<char> = s.chars().collect();
    if cs.windows(4).any(|w| w[0].is_lowercase() && w[1] == '-' && w[2] == ' ' && w[3].is_lowercase()) {
        out.push("split_line_end_hyphenation");
    }
    let l = s.to_lowercase();
    if EDITORIAL_MARKS.iter().any(|m| l.contains(m)) {
        out.push("editorial_bracket_note");
    }
    if folded_tokens(s).iter().any(|t| OCR_CONFUSION_TOKENS.contains(&t.as_str())) {
        out.push("ocr_confusion_token");
    }
    let digit_inside = s.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric());
        let c: Vec<char> = w.chars().collect();
        c.len() >= 3
            && c.iter().all(|x| x.is_alphanumeric())
            && c.windows(3).any(|x| x[0].is_lowercase() && x[1].is_ascii_digit() && x[2].is_lowercase())
    });
    if digit_inside {
        out.push("digit_inside_word");
    }
    out
}

/// The first strong transcription artefact, if any.
pub fn strong_transcription_artifact(s: &str) -> Option<&'static str> {
    transcription_artifacts(s).into_iter().find(|m| *m != "digit_inside_word")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str) -> Para {
        Para::classify(t, Screen::MARKUP)
    }
    fn sentence(n: usize) -> String {
        let mut s = vec!["word"; n].join(" ");
        s.push('.');
        s
    }

    #[test]
    fn hashes_follow_existing_method() {
        assert_eq!(normalized_sha("a  b\r\nc"), sha_hex(b"a b c"));
        assert_eq!(folded("It's  the END."), "it s the end");
        let t = (0..60).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        assert_eq!(shingles_ws(&t).len(), 11);
        assert_eq!(shingles_ws(&t)[0], h64((0..50).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ").as_bytes()));
        assert_eq!(shingles_folded(&t.to_uppercase()), shingles_folded(&t));
    }

    #[test]
    fn prose_screen_rejects_gaps_markup_and_quotes() {
        assert!(p(&sentence(20)).prose);
        assert_eq!(p("A short one.").reason, Some("short_paragraph"));
        assert_eq!(p(&format!("{} no end", sentence(10))).reason, Some("no_sentence_end"));
        assert_eq!(p(&format!("See {} [[x]].", sentence(9))).reason, Some("residual_markup"));
        assert_eq!(p(&format!("{GAP} {}", sentence(10))).reason, Some("removed_inline_content"));
        let q = format!("{} He said \"one two three four five six seven eight\".", sentence(5));
        assert_eq!(p(&q).reason, Some("long_quotation"));
        assert!(Para::classify(q, Screen { quotes: false, angle_brackets: true }).prose);
        assert!(Para::classify(format!("{} p < 0.05.", sentence(9)), Screen::XML).prose);
        assert_eq!(p(&format!("{} https://example.org.", sentence(9))).reason, Some("url"));
    }

    #[test]
    fn paragraph_runs_keep_whole_paragraphs_within_bounds() {
        // Complete short unit admitted whole.
        let short = vec![p(&sentence(90)), p(&sentence(70))];
        let e = paragraph_run(&short).unwrap();
        assert!(e.complete_unit && e.words == 160 && e.text.contains("\n\n"));
        // Stops at the first boundary at or after 220 words.
        let long = vec![p(&sentence(100)), p(&sentence(130)), p(&sentence(100))];
        let e = paragraph_run(&long).unwrap();
        assert_eq!((e.start, e.end, e.words, e.complete_unit), (0, 2, 230, false));
        // A first paragraph over the bound is skipped, never cropped.
        let skip = vec![p(&sentence(500)), p(&sentence(160))];
        let e = paragraph_run(&skip).unwrap();
        assert_eq!((e.start, e.end), (1, 2));
        // A non-prose break separates runs; a short leading run is skipped.
        let broken = vec![p(&sentence(60)), Para::non_prose("== H ==", "heading"), p(&sentence(155))];
        let e = paragraph_run(&broken).unwrap();
        assert_eq!((e.start, e.end, e.complete_unit), (2, 3, false));
        assert!(paragraph_run(&[p(&sentence(120))]).is_none());
    }

    #[test]
    fn pg_excerpt_matches_book_rule() {
        let body = (0..400).map(|_| "Alpha beta gamma delta. ").collect::<String>();
        let text = format!("Header\n*** START OF THE PROJECT GUTENBERG EBOOK X ***\n{body}\n*** END OF THE PROJECT GUTENBERG EBOOK X ***\n");
        let (s, e) = pg_excerpt(&text, 20).unwrap();
        let ex = &text[s..e];
        assert!(ex.starts_with("Alpha") && ex.ends_with('.'));
        assert_eq!(words(ex), 220);
        assert_eq!(pg_excerpt("no markers", 20), Err("no_pg_start_marker"));
    }

    fn pg_book(body: &str) -> String {
        format!("Header\n*** START OF THE PROJECT GUTENBERG EBOOK X ***\n{body}\n*** END OF THE PROJECT GUTENBERG EBOOK X ***\n")
    }

    fn plain_para(i: usize) -> String {
        let lines: Vec<String> = (0..4).map(|l| format!("Plain narrative line {i} {l} keeps going with words")).collect();
        format!("{}.\n", lines.join("\n"))
    }

    #[test]
    fn pg_v2_matches_v1_without_quotations() {
        let body = (0..400).map(|_| "Alpha beta gamma delta. ").collect::<String>();
        let text = pg_book(&body);
        for f in [10, 20, 50, 80] {
            let v1 = pg_excerpt(&text, f).unwrap();
            let v2 = pg_excerpt_v2(&text, f, true).unwrap();
            assert_eq!((v2.start, v2.end, v2.quotation_restarts), (v1.0, v1.1, 0));
        }
    }

    #[test]
    fn pg_v2_restarts_after_inline_and_block_quotations() {
        let mut body = String::new();
        for i in 0..120 {
            body.push_str(&plain_para(i));
            body.push('\n');
            if i == 30 {
                body.push_str("He wrote, \u{201c}one two three four five six seven eight nine\nten words of quoted matter here.\u{201d} Then more.\n\n");
            }
            if i == 33 {
                body.push_str("    An indented block quotation line with many words in it,\n    and a second indented line that closes the quoted verse.\n\n");
            }
        }
        let text = pg_book(&body);
        let (b, _) = pg_body(&text).unwrap();
        let t = &text[b..];
        let inline = pg_quotation_regions(&t[..t.find("*** END").unwrap()], true);
        assert_eq!(inline.iter().filter(|r| !r.block).count(), 1);
        assert_eq!(inline.iter().filter(|r| r.block).count(), 1);
        let fiction = pg_quotation_regions(&t[..t.find("*** END").unwrap()], false);
        assert!(fiction.iter().all(|r| r.block) && fiction.len() == 1);
        // Find a fraction whose v1 excerpt covers the quotation.
        let q = text.find('\u{201c}').unwrap();
        let f = (1..90).find(|&f| pg_excerpt(&text, f).is_ok_and(|(s, e)| s <= q && q < e)).expect("some excerpt covers the quote");
        let v2 = pg_excerpt_v2(&text, f, true).unwrap();
        assert!(v2.quotation_restarts >= 1);
        let ex = &text[v2.start..v2.end];
        assert!(!ex.contains('\u{201c}') && !ex.contains("    An indented"), "{ex}");
        assert!(ex.split_whitespace().count() >= 220);
        // Fiction keeps dialogue: no inline restart for the same excerpt unless a block overlaps.
        let v2f = pg_excerpt_v2(&text, f, false).unwrap();
        assert!(text[v2f.start..v2f.end].contains('\u{201c}') || v2f.quotation_restarts > 0);
    }

    #[test]
    fn pg_quotes_ignore_apostrophes_and_short_quotes() {
        let t = "She said \u{2018}don\u{2019}t go\u{2019} and \"no\" twice, then left the house quietly.\n";
        assert!(pg_quotation_regions(t, true).is_empty());
        let t2 = "It read \u{2018}we shan\u{2019}t ever return to the old house by the river again\u{2019} plainly.\n";
        assert_eq!(pg_quotation_regions(t2, true).len(), 1);
        // A book that indents every paragraph disables block detection.
        let all = (0..10).map(|i| format!("  Indented line {i} one two three four.\n  Indented line {i} five six seven.\n  Indented line {i} eight nine ten.\n\n")).collect::<String>();
        assert!(pg_quotation_regions(&all, false).is_empty());
    }

    #[test]
    fn english_screen_rejects_other_languages_and_scripts() {
        // Synthetic sentences only.
        let en = "The dog ran to the house and it was happy, so we gave him a bone in the yard by the door.";
        assert_eq!(english_screen(en), None);
        let fr = "Le chat noir dort sous la table pendant que les enfants jouent dans le jardin avec leurs amis.";
        assert_eq!(english_screen(fr), Some("english_stopword_share_below_0_20"));
        let el = "Ο σκύλος τρέχει στον κήπο και τα παιδιά γελούν δυνατά μέσα στο σπίτι, and the rest.";
        assert_eq!(english_screen(el), Some("latin_letter_share_below_0_90"));
        assert!(latin_letter_share("café naïve") == 1.0);
    }

    #[test]
    fn residue_screen_matches_post_render_rules() {
        assert!(markup_residue("A clean sentence (with words) and [brackets].").is_empty());
        assert_eq!(markup_residue("x | y"), vec!["pipe_markup_residue"]);
        assert_eq!(markup_residue("born ( ) in"), vec!["empty_parentheses_rendering_hole"]);
        assert!(markup_residue("f() call").is_empty(), "word char before the parenthesis");
        assert_eq!(markup_residue("a claim.[12] next"), vec!["literal_numeric_citation_marker"]);
        assert!(markup_residue("a [1234] code").is_empty());
        assert_eq!(markup_residue("open [ only"), vec!["unbalanced_square_brackets"]);
        let a = shingles_folded_n("one two three four five six", 3);
        assert_eq!(a.len(), 4);
        assert_ne!(shingles_folded_n("a b c", 3), shingles_folded_n("a b c", 2));
    }

    #[test]
    fn keys_match_existing_family_formats() {
        assert_eq!(author_key("Wharton, Edith (Edith Newbold)"), "edith_wharton");
        assert_eq!(title_key("The Custom of the Country: A Novel"), "custom_of_the_country");
        assert_eq!(
            title_aliases("The Declaration of Independence"),
            vec!["title:declarationofindependence", "title:thedeclarationofindependence"]
        );
    }

    #[test]
    fn transcription_artifacts_are_named_and_digit_words_only_recorded() {
        assert!(transcription_artifacts("A plain modern sentence about the weather and the sea.").is_empty());
        assert_eq!(transcription_artifacts("The houſe ſtood there."), vec!["long_s"]);
        assert_eq!(strong_transcription_artifact("a ﬁne day"), Some("typographic_ligature"));
        assert_eq!(strong_transcription_artifact("the exam- ple shows it"), Some("split_line_end_hyphenation"));
        assert_eq!(strong_transcription_artifact("pre-war and post-war years"), None);
        assert_eq!(strong_transcription_artifact("He wrote [sic] twice."), Some("editorial_bracket_note"));
        assert_eq!(strong_transcription_artifact("and tbe man went home"), Some("ocr_confusion_token"));
        assert_eq!(transcription_artifacts("the w0rd here"), vec!["digit_inside_word"]);
        assert_eq!(strong_transcription_artifact("the w0rd here"), None);
        assert!(transcription_artifacts("vitamin B12 and COVID-19 in 2020s").is_empty());
    }
}

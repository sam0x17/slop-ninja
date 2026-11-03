//! Page-level screens: URL and rights exclusions, Gopher quality and
//! repetition rules, versioned machine-text markers and personal-data
//! redaction. No detector score is used anywhere.
//!
//! Gopher rules follow Rae et al. (2021, appendix A) as implemented by
//! datatrove: whitespace words, words without any alphanumeric character
//! are symbols. WET pages have one text block per line and no blank-line
//! paragraphs, so the duplicate-paragraph rules equal the duplicate-line
//! rules and are not applied twice.
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

pub const SCREENS_VERSION: &str = "cc_human_page_screens_v2";
pub const MIN_PAGE_CHARS: usize = 1_000;
pub const MAX_PAGE_CHARS: usize = 1_000_000;

// ---------------------------------------------------------------- URL gates

/// Registered domains excluded at admission (in addition to FineWeb's
/// `excluded-registered-domains-v1`): data-policy exclusions, sites whose
/// text corpus-v1 already holds as its own source, fiction communities
/// that object to AI use, and adult story hosts found in the 2026-10-30
/// precision review.
pub const EXCLUDED_REGISTERED_DOMAINS: &[(&str, &str)] = &[
    ("reddit.com", "policy_excluded_source"),
    ("redd.it", "policy_excluded_source"),
    ("redditmedia.com", "policy_excluded_source"),
    ("goodreads.com", "policy_excluded_source"),
    ("archiveofourown.org", "fiction_community_objects_to_ai_use"),
    ("courtlistener.com", "corpus_v1_source_site"),
    ("amazon.com", "policy_excluded_reviews_host"),
    ("amazon.co.uk", "policy_excluded_reviews_host"),
    ("amazon.ca", "policy_excluded_reviews_host"),
    ("amazon.com.au", "policy_excluded_reviews_host"),
    ("literotica.com", "adult_host"),
    ("adult-fanfiction.org", "adult_host"),
    ("nifty.org", "adult_host"),
    ("storiesonline.net", "adult_host"),
    ("asstr.org", "adult_host"),
    ("sexstories.com", "adult_host"),
    ("lushstories.com", "adult_host"),
    ("xossip.com", "adult_host"),
    ("thekinkyserver.com", "adult_host"),
    ("adultfriendfinder.com", "adult_host"),
];
/// Exact hosts excluded (the registered domain itself stays eligible).
pub const EXCLUDED_HOSTS: &[(&str, &str)] = &[
    ("news.ycombinator.com", "policy_excluded_source"),
    ("law.justia.com", "corpus_v1_source_site"),
];
/// Registered-domain labels excluded under any public suffix (yelp.com, yelp.co.uk, ...).
pub const EXCLUDED_DOMAIN_LABELS: &[(&str, &str)] = &[("yelp", "policy_excluded_source")];

/// In-house adult-host screen: host tokens (split on `.`, `-` and digits).
const ADULT_TOKENS: &[&str] = &[
    "porn", "porno", "xxx", "sex", "sexy", "escort", "escorts", "hentai", "nude", "nudes", "milf", "fetish", "erotic", "erotica", "camgirl",
    "camgirls", "xvideos", "xhamster", "redtube", "youporn", "pornhub", "tube8", "bdsm", "nsfw", "sexcam", "swinger", "swingers", "boobs", "pussy",
    "shemale", "incest",
];
const ADULT_PREFIXES: &[&str] = &["porn", "xxx", "sexy", "sexcam", "hentai", "erotic", "fetish", "nude"];

/// Substrings that mark an adult host anywhere inside a host label
/// (`thepornobest`, `reviewedporn`, `trannyphonesexchicks`). Words that
/// occur inside ordinary names (`sex` in Sussex, `erotic` in sclerotic,
/// `kinky` in hair care) are tokens or prefixes only.
const ADULT_SUBSTRINGS: &[&str] = &[
    "porn", "xxx", "hentai", "xvideo", "xnxx", "xhamster", "phonesex", "camgirl", "sexstor", "nsfw",
];

pub fn adult_host(host: &str) -> bool {
    let toks = host.split(|c: char| c == '.' || c == '-' || c.is_ascii_digit()).filter(|t| !t.is_empty());
    for t in toks {
        if ADULT_TOKENS.contains(&t) || ADULT_PREFIXES.iter().any(|p| t.starts_with(p)) || ADULT_SUBSTRINGS.iter().any(|p| t.contains(p)) {
            return true;
        }
    }
    false
}

/// Essay-mill hosts (heavy spinning, plagiarism and copied encyclopedia
/// text; the sampler excludes them): any host label containing `essay`.
pub fn essay_mill_host(host: &str) -> bool {
    host.split('.').any(|l| l.contains("essay"))
}

/// Translated web-novel and manga aggregators: a chapter page on a host
/// whose labels name novels, manga or translations. They are unlicensed
/// copies or translations (rights and machine-translation risk), never
/// original English prose.
pub fn translation_aggregator(host: &str, path: &str) -> bool {
    const HOST: &[&str] = &["novel", "manga", "manhwa", "manhua", "wuxia", "xianxia", "webtoon", "translat"];
    let p = path.to_ascii_lowercase();
    p.contains("chapter") && host.split('.').any(|l| !l.starts_with("novell") && HOST.iter().any(|h| l.contains(h)))
}

pub fn url_exclusion(host: &str, domain: &str, fineweb_excluded: &HashSet<String>) -> Option<&'static str> {
    if let Some((_, r)) = EXCLUDED_HOSTS.iter().find(|(h, _)| *h == host) {
        return Some(r);
    }
    if fineweb_excluded.contains(domain) {
        return Some("registered_domain_of_another_pool_source");
    }
    if let Some((_, r)) = EXCLUDED_REGISTERED_DOMAINS.iter().find(|(d, _)| *d == domain) {
        return Some(r);
    }
    let label = domain.split('.').next().unwrap_or("");
    if let Some((_, r)) = EXCLUDED_DOMAIN_LABELS.iter().find(|(l, _)| *l == label) {
        return Some(r);
    }
    if adult_host(host) {
        return Some("adult_host");
    }
    if essay_mill_host(host) {
        return Some("essay_mill_host");
    }
    None
}

/// Path screens: legal boilerplate pages (privacy, terms, EULA, cookie and
/// disclaimer pages repeat templates across sites), HTML pages served from
/// upload folders (spam drops) and chapter pages of translation
/// aggregators.
pub fn path_exclusion(host: &str, path: &str) -> Option<&'static str> {
    static POLICY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)/(privacy|privacy[-_]?policy|privacy[-_]?notice|terms|terms[-_]?(of[-_]?(service|use|sale))|terms[-_]?(and|&)[-_]?conditions|termsandconditions|tos|eula|disclaimer|cookie[-_]?policy|legal[-_]?notice|policy[-_]?\d*)(\.[a-z]+)?/?([?#].*)?$",
        )
        .unwrap()
    });
    static UPLOAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)/wp-content/uploads/.*\.(html?|php)([?#].*)?$").unwrap());
    if POLICY.is_match(path) {
        return Some("boilerplate_policy_page");
    }
    if UPLOAD.is_match(path) {
        return Some("spam_upload_page");
    }
    if translation_aggregator(host, path) {
        return Some("translation_aggregator");
    }
    None
}

pub const EXCERPT_SCREENS_VERSION: &str = "cc_human_excerpt_screens_v1";

/// Explicit sexual vocabulary. Medical and profane words that ordinary
/// forum and health text uses (penis, sex, fuck, anus) and words common in
/// news and engineering text (erect, thrust) are not listed.
const EXPLICIT: &[&str] = &[
    "cock",
    "cocks",
    "pussy",
    "cunt",
    "clit",
    "tits",
    "titties",
    "blowjob",
    "handjob",
    "cum",
    "cumming",
    "cumshot",
    "dildo",
    "horny",
    "orgasm",
    "orgasms",
    "moaned",
    "moaning",
    "moans",
    "fucked",
    "fucking her",
    "fucking him",
    "nipples",
    "stroking",
    "her ass",
    "his dick",
    "sucking",
    "wet pussy",
    "hard cock",
    "milf",
    "porn",
];

/// Distinct explicit terms (whole words, ASCII case-insensitive).
pub fn explicit_terms(text: &str) -> usize {
    static AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
        AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .match_kind(MatchKind::Standard)
            .build(EXPLICIT)
            .unwrap()
    });
    let b = text.as_bytes();
    let mut seen = vec![false; EXPLICIT.len()];
    for m in AC.find_overlapping_iter(text) {
        if bounded(b, m.start(), m.end()) {
            seen[m.pattern().as_usize()] = true;
        }
    }
    seen.iter().filter(|x| **x).count()
}

/// Excerpt-level screens: sexually explicit text (three or more distinct
/// explicit terms in the excerpt).
pub fn excerpt_exclusion(excerpt: &str) -> Option<&'static str> {
    (explicit_terms(excerpt) >= 3).then_some("adult_text")
}

// ---------------------------------------------------------------- Gopher

/// Fast non-cryptographic word hash (FNV-1a 64); collisions only merge
/// counts.
pub fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

const GOPHER_STOPWORDS: &[&str] = &["the", "be", "to", "of", "and", "that", "have", "with"];
const BULLETS: &[char] = &[
    '\u{2022}', '\u{2023}', '\u{25b6}', '\u{25cf}', '\u{25e6}', '\u{25aa}', '\u{25ab}', '\u{2013}', '\u{2043}', '-', '*',
];

fn is_symbol_word(w: &str) -> bool {
    !w.chars().any(|c| c.is_alphanumeric())
}

pub fn gopher_quality(text: &str) -> Option<&'static str> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let non_symbol: Vec<&str> = words.iter().copied().filter(|w| !is_symbol_word(w)).collect();
    let n = non_symbol.len();
    if n < 50 {
        return Some("gopher_below_50_words");
    }
    if n > 100_000 {
        return Some("gopher_above_100000_words");
    }
    let mean = non_symbol.iter().map(|w| w.chars().count()).sum::<usize>() as f64 / n as f64;
    if !(3.0..=10.0).contains(&mean) {
        return Some("gopher_mean_word_length");
    }
    let symbols = text.matches('#').count() + text.matches("...").count() + text.matches('\u{2026}').count();
    if symbols as f64 / words.len().max(1) as f64 > 0.1 {
        return Some("gopher_symbol_to_word_ratio");
    }
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let nl = lines.len().max(1) as f64;
    if lines.iter().filter(|l| l.starts_with(BULLETS)).count() as f64 / nl > 0.9 {
        return Some("gopher_bullet_lines");
    }
    if lines.iter().filter(|l| l.ends_with("...") || l.ends_with('\u{2026}')).count() as f64 / nl > 0.3 {
        return Some("gopher_ellipsis_lines");
    }
    let alpha = words.iter().filter(|w| w.chars().any(|c| c.is_alphabetic())).count();
    if (alpha as f64) / (words.len().max(1) as f64) < 0.8 {
        return Some("gopher_alphabetic_words");
    }
    let mut seen = [false; 8];
    for w in &words {
        let l = w.to_lowercase();
        if let Some(i) = GOPHER_STOPWORDS.iter().position(|s| *s == l) {
            seen[i] = true;
        }
    }
    if seen.iter().filter(|x| **x).count() < 2 {
        return Some("gopher_stop_words");
    }
    None
}

const TOP_NGRAM: &[(usize, f64)] = &[(2, 0.20), (3, 0.18), (4, 0.16)];
const DUP_NGRAM: &[(usize, f64)] = &[(5, 0.15), (6, 0.14), (7, 0.13), (8, 0.12), (9, 0.11), (10, 0.10)];

pub fn gopher_repetition(text: &str) -> Option<&'static str> {
    let lines: Vec<&str> = text.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let total_chars = text.chars().count().max(1) as f64;
    let mut seen: HashSet<u64> = HashSet::with_capacity(lines.len());
    let (mut dup_lines, mut dup_chars) = (0usize, 0usize);
    for l in &lines {
        if !seen.insert(fnv(l)) {
            dup_lines += 1;
            dup_chars += l.chars().count();
        }
    }
    if dup_lines as f64 / lines.len().max(1) as f64 > 0.30 {
        return Some("gopher_duplicate_line_fraction");
    }
    if dup_chars as f64 / total_chars > 0.20 {
        return Some("gopher_duplicate_line_char_fraction");
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let wh: Vec<u64> = words.iter().map(|w| fnv(w)).collect();
    let wl: Vec<usize> = words.iter().map(|w| w.chars().count()).collect();
    let ngram_hash = |i: usize, n: usize| -> u64 {
        let mut h: u64 = n as u64;
        for k in 0..n {
            h = h.wrapping_mul(0x9E3779B97F4A7C15).rotate_left(31) ^ wh[i + k];
        }
        h
    };
    let ngram_chars = |i: usize, n: usize| -> usize { wl[i..i + n].iter().sum::<usize>() + n - 1 };
    for &(n, max) in TOP_NGRAM {
        if words.len() < n {
            continue;
        }
        let mut counts: HashMap<u64, (u32, usize)> = HashMap::new();
        for i in 0..=words.len() - n {
            let e = counts.entry(ngram_hash(i, n)).or_insert((0, ngram_chars(i, n)));
            e.0 += 1;
        }
        if let Some((c, len)) = counts.values().filter(|(c, _)| *c > 1).max_by_key(|(c, len)| (*c, *len))
            && (*c as usize * len) as f64 / total_chars > max
        {
            return Some(match n {
                2 => "gopher_top_2gram_char_share",
                3 => "gopher_top_3gram_char_share",
                _ => "gopher_top_4gram_char_share",
            });
        }
    }
    for &(n, max) in DUP_NGRAM {
        if words.len() < n {
            continue;
        }
        let mut seen: HashSet<u64> = HashSet::new();
        let (mut i, mut rep) = (0usize, 0usize);
        while i + n <= words.len() {
            let h = ngram_hash(i, n);
            if seen.contains(&h) {
                rep += ngram_chars(i, n);
                i += n;
            } else {
                seen.insert(h);
                i += 1;
            }
        }
        if rep as f64 / total_chars > max {
            return Some(match n {
                5 => "gopher_duplicate_5gram_char_share",
                6 => "gopher_duplicate_6gram_char_share",
                7 => "gopher_duplicate_7gram_char_share",
                8 => "gopher_duplicate_8gram_char_share",
                9 => "gopher_duplicate_9gram_char_share",
                _ => "gopher_duplicate_10gram_char_share",
            });
        }
    }
    None
}

// ---------------------------------------------------------- machine text

pub const MACHINE_TEXT_VERSION: &str = "cc_human_machine_text_markers_v1";

/// (rule id, case-insensitive substring). The first matching rule names
/// the exclusion; every matching rule is counted.
pub const MACHINE_TEXT_MARKERS: &[(&str, &str)] = &[
    ("mt_marker", "machine translated"),
    ("mt_marker", "machine-translated"),
    ("mt_marker", "machine translation of"),
    ("mt_marker", "translated by google"),
    ("mt_marker", "powered by google translate"),
    ("mt_marker", "google translate widget"),
    ("mt_marker", "automatically translated"),
    ("mt_marker", "auto-translated"),
    ("mt_marker", "contribute a better translation"),
    ("mt_marker", "translated with deepl"),
    ("mt_marker", "translated by bing"),
    ("mt_marker", "mtl novel"),
    ("mt_marker", "machine translation lite"),
    ("automated_text_marker", "automatically generated"),
    ("automated_text_marker", "auto-generated"),
    ("automated_text_marker", "autogenerated"),
    ("automated_text_marker", "automated insights"),
    ("automated_text_marker", "generated by wordsmith"),
    ("automated_text_marker", "narrative science"),
    ("automated_text_marker", "this story was generated"),
    ("automated_text_marker", "this article was generated"),
    ("automated_text_marker", "this content was generated"),
    ("automated_text_marker", "this report was generated"),
    ("automated_text_marker", "generated by an algorithm"),
    ("automated_text_marker", "computer-generated text"),
    ("automated_text_marker", "computer generated text"),
    ("automated_text_marker", "machine-generated text"),
    ("automated_text_marker", "machine generated text"),
    ("automated_text_marker", "machine-generated content"),
    ("ai_writing_tool_marker", "generated by ai"),
    ("ai_writing_tool_marker", "ai-generated"),
    ("ai_writing_tool_marker", "ai generated"),
    ("ai_writing_tool_marker", "written by ai"),
    ("ai_writing_tool_marker", "written by an ai"),
    ("ai_writing_tool_marker", "written by gpt"),
    ("ai_writing_tool_marker", "generated by gpt"),
    ("ai_writing_tool_marker", "gpt-2"),
    ("ai_writing_tool_marker", "gpt-3"),
    ("ai_writing_tool_marker", "gpt2"),
    ("ai_writing_tool_marker", "gpt3"),
    ("ai_writing_tool_marker", "articleforge"),
    ("ai_writing_tool_marker", "article forge"),
    ("ai_writing_tool_marker", "jasper.ai"),
    ("ai_writing_tool_marker", "copy.ai"),
    ("ai_writing_tool_marker", "conversion.ai"),
    ("ai_writing_tool_marker", "rytr"),
    ("ai_writing_tool_marker", "writesonic"),
    ("ai_writing_tool_marker", "shortlyai"),
    ("spinner_tool_marker", "spin rewriter"),
    ("spinner_tool_marker", "spinbot"),
    ("spinner_tool_marker", "wordai"),
    ("spinner_tool_marker", "quillbot"),
    ("spinner_tool_marker", "article spinner"),
    ("spinner_tool_marker", "article rewriter"),
    ("spinner_tool_marker", "paraphrasing tool"),
    ("asr_marker", "machine-generated transcript"),
    ("asr_marker", "automated transcript"),
    ("asr_marker", "auto-generated transcript"),
    ("asr_marker", "automatic transcript"),
    ("asr_marker", "automatically transcribed"),
    ("asr_marker", "transcribed by otter"),
    ("asr_marker", "otter.ai"),
    ("asr_marker", "temi.com"),
    ("asr_marker", "rev.ai"),
    ("asr_marker", "trint"),
    ("asr_marker", "descript"),
    ("asr_marker", "automatic captions"),
    ("asr_marker", "auto-captions"),
    ("asr_marker", "webvtt"),
];

static MARKERS: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .match_kind(MatchKind::Standard)
        .build(MACHINE_TEXT_MARKERS.iter().map(|(_, p)| *p))
        .expect("markers")
});
static SPINTAX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^{}|\n]{1,60}\|[^{}|\n]{1,60}(\|[^{}\n]{1,60})*\}").unwrap());
static SRT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d\d:\d\d:\d\d[,.]\d{3} --> \d\d:\d\d:\d\d").unwrap());

/// Whole-word marker check: a marker counts only when it is not part of a
/// longer alphanumeric token (so "trint" does not match "trinity").
fn bounded(t: &[u8], s: usize, e: usize) -> bool {
    let left = s == 0 || !t[s - 1].is_ascii_alphanumeric();
    let right = e >= t.len() || !t[e].is_ascii_alphanumeric();
    left && right
}

/// Every machine-text rule that fires on a page (rule ids, sorted, unique).
/// Spintax needs two `{a|b}` groups and is not applied to technical pages,
/// where brace alternations are command syntax.
pub fn machine_text(text: &str, technical: bool) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let b = text.as_bytes();
    for m in MARKERS.find_overlapping_iter(text) {
        if bounded(b, m.start(), m.end()) {
            out.push(MACHINE_TEXT_MARKERS[m.pattern().as_usize()].0);
        }
    }
    if !technical && SPINTAX.find_iter(text).nth(1).is_some() {
        out.push("spintax_residue");
    }
    if SRT.is_match(text) {
        out.push("asr_caption_timestamps");
    }
    if keyword_stuffing(text) {
        out.push("keyword_stuffing");
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A non-stopword folded unigram above 8% of the page's tokens (pages of at
/// least 100 tokens).
pub fn keyword_stuffing(text: &str) -> bool {
    let toks = grammar_a_pools::text::folded_tokens(text);
    if toks.len() < 100 {
        return false;
    }
    let mut c: HashMap<&str, usize> = HashMap::new();
    for t in &toks {
        if t.len() >= 3 && !t.chars().all(|c| c.is_ascii_digit()) && !STUFFING_STOP.contains(&t.as_str()) {
            *c.entry(t.as_str()).or_default() += 1;
        }
    }
    c.values().any(|&n| n as f64 > 0.08 * toks.len() as f64)
}

const STUFFING_STOP: &[&str] = &[
    "the", "and", "for", "that", "with", "you", "this", "are", "was", "but", "not", "have", "from", "they", "his", "her", "she", "him", "had", "all",
    "can", "will", "one", "our", "out", "your", "their", "there", "what", "when", "who", "which", "were", "been", "has", "its", "would", "about",
    "them", "then", "than", "into", "more", "some", "just",
];

// ------------------------------------------------------------ rights text

pub const RIGHTS_TEXT_VERSION: &str = "cc_human_rights_text_v1";

/// A non-commercial Creative Commons statement in the page text.
pub fn nc_licence(text: &str) -> bool {
    static AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
        AhoCorasickBuilder::new()
            .ascii_case_insensitive(true)
            .build([
                "creative commons",
                "creativecommons",
                "cc by-nc",
                "cc-by-nc",
                "by-nc-sa",
                "by-nc-nd",
                "noncommercial",
                "non-commercial",
            ])
            .unwrap()
    });
    let mut cc = false;
    let mut nc = false;
    for m in AC.find_overlapping_iter(text) {
        match m.pattern().as_usize() {
            0 | 1 => cc = true,
            2..=5 => return true,
            _ => nc = true,
        }
    }
    cc && nc
}

// --------------------------------------------------------------- PII

pub const REDACTION_VERSION: &str = "cc_human_redaction_v1";
static EMAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap());
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\+?\d{1,3}[\s.-]?)?(?:\(\d{3}\)|\d{3})[\s.-]\d{3}[\s.-]\d{4}\b|\+\d{1,3}(?:[\s.-]\d{2,4}){2,4}\b").unwrap());

/// Replace e-mail addresses and phone numbers with `[EMAIL]` / `[PHONE]`.
pub fn redact(text: &str) -> (String, usize, usize) {
    let e = EMAIL.find_iter(text).count();
    let t = EMAIL.replace_all(text, "[EMAIL]");
    let p = PHONE.find_iter(&t).count();
    let t = PHONE.replace_all(&t, "[PHONE]");
    (t.into_owned(), e, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prose(n: usize) -> String {
        let s = "The quick brown fox jumps over the lazy dog and runs to the river with a friend. ";
        s.repeat(n.div_ceil(17))
    }

    #[test]
    fn gopher_quality_rules() {
        assert_eq!(gopher_quality("too short"), Some("gopher_below_50_words"));
        let mut t = String::new();
        for i in 0..80 {
            t.push_str(&format!(
                "Line {i} of a varied page has words about the topic and that matters to readers.\n"
            ));
        }
        assert_eq!(gopher_quality(&t), None);
        let hashes = "# # # # # # # # # # word ".repeat(10);
        assert_eq!(gopher_quality(&hashes), Some("gopher_below_50_words"));
        let no_stop = "alpha beta gamma delta epsilon zeta theta iota kappa lambda ".repeat(10);
        assert_eq!(gopher_quality(&no_stop), Some("gopher_stop_words"));
    }

    #[test]
    fn gopher_repetition_rules() {
        let mut ok = String::new();
        let mut x: u64 = 7;
        for _ in 0..60 {
            let mut line = Vec::new();
            for _ in 0..10 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                line.push(format!("w{}", x >> 50));
            }
            ok.push_str(&format!("{}.\n", line.join(" ")));
        }
        assert_eq!(gopher_repetition(&ok), None);
        let dup = "Same line again and again here.\n".repeat(20);
        assert_eq!(gopher_repetition(&dup), Some("gopher_duplicate_line_fraction"));
        assert!(gopher_repetition(&prose(400)).is_some());
    }

    #[test]
    fn exclusions_and_markers() {
        let fw: HashSet<String> = ["wikipedia.org".to_owned()].into();
        assert_eq!(
            url_exclusion("en.wikipedia.org", "wikipedia.org", &fw),
            Some("registered_domain_of_another_pool_source")
        );
        assert_eq!(url_exclusion("www.yelp.co.uk", "yelp.co.uk", &fw), Some("policy_excluded_source"));
        assert_eq!(
            url_exclusion("news.ycombinator.com", "ycombinator.com", &fw),
            Some("policy_excluded_source")
        );
        assert_eq!(url_exclusion("www.ycombinator.com", "ycombinator.com", &fw), None);
        assert_eq!(url_exclusion("free-porn-site.com", "free-porn-site.com", &fw), Some("adult_host"));
        assert_eq!(url_exclusion("www.essex.ac.uk", "essex.ac.uk", &fw), None);
        // v2: substrings inside one label, listed story hosts, essay mills.
        for h in [
            "thepornobest.example",
            "www.reviewedporn.example",
            "trannyphonesexchicks.example",
            "a.sexstories.example",
        ] {
            assert_eq!(url_exclusion(h, h, &fw), Some("adult_host"), "{h}");
        }
        assert_eq!(url_exclusion("www.nifty.org", "nifty.org", &fw), Some("adult_host"));
        for h in [
            "www.sussexcricket.example",
            "warsclerotic.example",
            "kinkyhaircare.example",
            "foodnetwork.example",
        ] {
            assert_eq!(url_exclusion(h, h, &fw), None, "{h}");
        }
        assert_eq!(
            url_exclusion("www.bestessay-writers.example", "bestessay-writers.example", &fw),
            Some("essay_mill_host")
        );
        assert_eq!(
            url_exclusion("archiveofourown.org", "archiveofourown.org", &fw),
            Some("fiction_community_objects_to_ai_use")
        );
        assert_eq!(path_exclusion("shop.example", "/privacy-policy/"), Some("boilerplate_policy_page"));
        assert_eq!(
            path_exclusion("shop.example", "/termsandconditions.aspx"),
            Some("boilerplate_policy_page")
        );
        assert_eq!(path_exclusion("shop.example", "/pt/policy_14811.html"), Some("boilerplate_policy_page"));
        assert_eq!(path_exclusion("shop.example", "/eula/"), Some("boilerplate_policy_page"));
        assert_eq!(path_exclusion("bakery.example", "/recipes/cookies/"), None);
        assert_eq!(path_exclusion("shop.example", "/cookie-policy"), Some("boilerplate_policy_page"));
        assert_eq!(path_exclusion("blog.example", "/2015/03/our-new-privacy-rules-explained/"), None);
        assert_eq!(path_exclusion("blog.example", "/policy/analysis-of-the-budget"), None);
        assert_eq!(
            path_exclusion("x.example", "/wp-content/uploads/2019/01/how-to-buy.html"),
            Some("spam_upload_page")
        );
        assert_eq!(path_exclusion("x.example", "/wp-content/uploads/2019/01/photo.jpg"), None);
        assert_eq!(
            path_exclusion("anylightnovel.example", "/a-tale/chapter-12"),
            Some("translation_aggregator")
        );
        assert_eq!(
            path_exclusion("someteamtranslations.example", "/book/chapter-3/"),
            Some("translation_aggregator")
        );
        assert_eq!(path_exclusion("novelist-blog.example", "/about/"), None);
        assert_eq!(path_exclusion("www.novell.example", "/documentation/chapter-3/"), None);
        assert_eq!(path_exclusion("www.example.com", "/book/chapter-3/"), None);
        let explicit = "She moaned as his hard cock pressed on her tits.";
        assert_eq!(excerpt_exclusion(explicit), Some("adult_text"));
        let medical = "Unprotected sex can spread infection; check the penis and anus, and see a doctor about any rash.";
        assert_eq!(excerpt_exclusion(medical), None);
        let profane = "That trade was fucking stupid and the owners are full of shit, cock-eyed as ever.";
        assert_eq!(excerpt_exclusion(profane), None);
        let news = "Engineers erect the tower as the thrust of the debate over porn filters and nipples on statues continues.";
        assert_eq!(excerpt_exclusion(news), None);
        assert_eq!(machine_text("This page was Machine Translated from Spanish.", false), vec!["mt_marker"]);
        assert_eq!(machine_text("Trinity college library notes.", false), Vec::<&str>::new());
        let spun = "Buy {cheap|low cost|affordable} shoes {today|now}.";
        assert_eq!(machine_text(spun, false), vec!["spintax_residue"]);
        assert!(machine_text(spun, true).is_empty());
        assert!(nc_licence("Licensed under a Creative Commons Attribution-NonCommercial 4.0 licence."));
        assert!(!nc_licence("Licensed under a Creative Commons Attribution 4.0 licence."));
        let (t, e, p) = redact("Mail me at jo.smith@example.com or call (555) 123-4567 today.");
        assert_eq!((t.as_str(), e, p), ("Mail me at [EMAIL] or call [PHONE] today.", 1, 1));
    }
}

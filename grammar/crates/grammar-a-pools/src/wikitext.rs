//! Conservative wikitext rendering into whole paragraphs.
//!
//! Deliberately partial, like the historical Wikipedia screen it extends:
//! no template is expanded. A template, element or link that would have
//! rendered text inline leaves a gap marker, so its paragraph can never be
//! admitted as prose; only known non-text inline templates (citation
//! markers and similar) are removed silently. Block-level removals become
//! non-prose paragraph breaks.
use crate::text::{GAP, Para, Screen, squash};

/// Marks a removed block-level construct; renders as a non-prose paragraph.
pub const BLOCK: char = '\u{E001}';

pub const RENDER_METHOD: &str = "slop_ninja_conservative_wikitext_paragraphs_v1";

#[derive(Clone, Debug)]
pub struct Section {
    pub heading: Option<String>,
    pub level: usize,
    pub paras: Vec<Para>,
}

/// Inline templates that render no prose (citation and maintenance marks).
fn silent_template(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "cn", "citation needed", "fact", "sfn", "sfnp", "sfnm", "harvnb", "efn", "efn-ua",
        "efn-lr", "efn-lg", "refn", "r", "rp", "ref label", "note label", "dubious", "clarify",
        "when", "who", "which", "by whom", "failed verification", "better source needed",
        "better source", "according to whom", "page needed", "vague", "self-published inline",
        "unreliable source?", "dead link", "verification needed", "original research inline",
        "or", "citation needed lead", "update inline", "as of?", "specify", "examples",
        "full citation needed", "irrelevant citation", "primary source inline", "third-party inline",
        "toc", "toc limit", "clear", "-", "anchor", "short description", "good article",
        "featured article", "authority control", "portal", "commons category", "reflist",
        "dmy", "mdy", "engvarb", "italic title", "lowercase title", "dts",
    ];
    NAMES.contains(&name)
        || name.starts_with("use ")
        || name.starts_with("pp-")
        || name == "pp"
        || name.starts_with("dmy")
}

fn template_name(inner: &str) -> String {
    inner
        .split(['|', '}', '\n'])
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches(':')
        .replace('_', " ")
        .to_lowercase()
        .trim_start_matches("template:")
        .trim()
        .to_owned()
}

/// Is the removed region [i, j) block-level within `s`?
/// Text before it on the same line makes a removal inline; otherwise it is
/// block-level if it spans lines or nothing follows it on its line.
fn block_context(s: &str, i: usize, j: usize) -> bool {
    let before = s[..i].rsplit('\n').next().unwrap_or("");
    if !before.chars().all(|c| c.is_whitespace() || c == BLOCK) {
        return false;
    }
    let after = s[j..].split('\n').next().unwrap_or("");
    s[i..j].contains('\n') || after.trim().is_empty()
}

fn find_ci(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return None;
    }
    (from..=h.len().saturating_sub(n.len())).find(|&k| h[k..k + n.len()].eq_ignore_ascii_case(n))
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Remove `<name ...>...</name>` and `<name .../>`. `silent` removals leave
/// nothing; others leave a gap (inline) or a block marker.
fn strip_element(s: &str, name: &str, silent: bool) -> String {
    let open = format!("<{name}");
    let close = format!("</{name}");
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    while let Some(i) = find_ci(s, &open, pos) {
        // Require a tag boundary after the name.
        let after = s[i + open.len()..].chars().next();
        if !matches!(after, Some(' ' | '>' | '/' | '\t' | '\n')) {
            out.push_str(&s[pos..i + open.len()]);
            pos = i + open.len();
            continue;
        }
        out.push_str(&s[pos..i]);
        let Some(gt) = s[i..].find('>') else {
            out.push(GAP);
            return out;
        };
        let tag_end = i + gt + 1;
        let end = if s[i..tag_end].ends_with("/>") {
            tag_end
        } else {
            match find_ci(s, &close, tag_end) {
                Some(c) => c + s[c..].find('>').map(|k| k + 1).unwrap_or(s.len() - c),
                None => s.len(),
            }
        };
        if !silent {
            out.push(if block_context(s, i, end) { BLOCK } else { GAP });
        }
        pos = end;
    }
    out.push_str(&s[pos..]);
    out
}

/// Remove templates and tables with nesting.
fn strip_templates(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        let line_start = i == 0 || b[i - 1] == b'\n';
        let table = line_start && b[i..].starts_with(b"{|");
        if b[i..].starts_with(b"{{") || table {
            out.push_str(&s[copied..i]);
            let (open, close): (&[u8], &[u8]) = if table { (b"{|", b"|}") } else { (b"{{", b"}}") };
            let mut depth = 0usize;
            let mut j = i;
            let mut end = None;
            while j < b.len() {
                if b[j..].starts_with(open) && (!table || j == 0 || b[j - 1] == b'\n') {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(close) && (!table || j == 0 || b[j - 1] == b'\n') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            let Some(end) = end else {
                out.push(GAP);
                return out;
            };
            let name = if table { String::new() } else { template_name(&s[i + 2..end - 2]) };
            if table || block_context(s, i, end) {
                if table || !silent_template(&name) {
                    out.push(BLOCK);
                }
            } else if name == "nbsp" || name == "sp" {
                out.push(' ');
            } else if !silent_template(&name) {
                out.push(GAP);
            }
            i = end;
            copied = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[copied..]);
    out
}

fn namespace_link(target: &str) -> bool {
    let t = target.trim_start().to_lowercase();
    if t.starts_with(':') {
        return false;
    }
    for ns in ["file:", "image:", "media:", "category:"] {
        if t.starts_with(ns) {
            return true;
        }
    }
    // Interlanguage links such as [[fr:...]].
    match t.split_once(':') {
        Some((p, _)) => (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_lowercase()),
        None => false,
    }
}

/// Render internal and external links.
fn render_links(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i..].starts_with(b"[[") {
            let mut depth = 0usize;
            let mut j = i;
            let mut end = None;
            while j + 1 < b.len() {
                if b[j..].starts_with(b"[[") {
                    depth += 1;
                    j += 2;
                } else if b[j..].starts_with(b"]]") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            let Some(end) = end else { break };
            out.push_str(&s[copied..i]);
            let inner = &s[i + 2..end - 2];
            if namespace_link(inner) {
                if block_context(s, i, end) {
                    out.push(BLOCK);
                }
            } else if inner.contains("[[") {
                out.push(GAP);
            } else {
                let label = match inner.split_once('|') {
                    Some((_, l)) => l,
                    None => inner.trim_start_matches(':'),
                };
                if label.trim().is_empty() {
                    out.push(GAP);
                } else {
                    out.push_str(label);
                }
            }
            i = end;
            copied = end;
        } else if b[i] == b'[' {
            let rest = &s[i + 1..];
            let lower: String = rest.chars().take(8).collect::<String>().to_lowercase();
            if ["http://", "https://", "//", "ftp://", "mailto:"].iter().any(|p| lower.starts_with(p)) {
                if let Some(close) = rest.find(']') {
                    out.push_str(&s[copied..i]);
                    match rest[..close].split_once(' ') {
                        Some((_, label)) if !label.trim().is_empty() => out.push_str(label),
                        _ => out.push(GAP),
                    }
                    i = i + 1 + close + 1;
                    copied = i;
                    continue;
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// Tags whose markup is dropped but whose content renders as ordinary text.
const KEEP_CONTENT: &[&str] = &[
    "span", "small", "big", "b", "i", "u", "em", "strong", "abbr", "cite", "dfn", "nowiki",
    "var", "code", "kbd", "samp", "q", "bdi", "font", "center", "div", "p", "onlyinclude",
    "includeonly", "noinclude", "section",
];

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let name: String = tail
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let is_tag = !name.is_empty() && tail.chars().next().is_some_and(|c| c == '/' || c.is_ascii_alphabetic());
        match (is_tag, tail.find('>')) {
            (true, Some(gt)) => {
                if !KEEP_CONTENT.contains(&name.to_lowercase().as_str()) {
                    out.push(GAP);
                }
                rest = &tail[gt + 1..];
            }
            _ => {
                out.push('<');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let end = tail
            .char_indices()
            .take(12)
            .find(|&(_, c)| c == ';')
            .map(|(k, _)| k)
            .filter(|&k| k > 0);
        let decoded = end.and_then(|k| entity(&tail[..k]));
        match (end, decoded) {
            (Some(k), Some(c)) => {
                out.push(c);
                rest = &tail[k + 1..];
            }
            (Some(k), None) if tail[..k].chars().all(|c| c.is_ascii_alphanumeric() || c == '#') => {
                out.push(GAP);
                rest = &tail[k + 1..];
            }
            _ => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A named or numeric character reference (without `&` and `;`).
pub fn entity(name: &str) -> Option<char> {
    if let Some(n) = name.strip_prefix('#') {
        let v = match n.strip_prefix(['x', 'X']) {
            Some(h) => u32::from_str_radix(h, 16).ok()?,
            None => n.parse().ok()?,
        };
        return char::from_u32(v).map(|c| if c == '\u{a0}' { ' ' } else { c });
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" | "ensp" | "emsp" | "thinsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "minus" => '−',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "middot" => '·',
        "deg" => '°',
        "eacute" => 'é',
        "egrave" => 'è',
        "aacute" => 'á',
        "oacute" => 'ó',
        "uuml" => 'ü',
        "ouml" => 'ö',
        "auml" => 'ä',
        "ccedil" => 'ç',
        "times" => '×',
        "pound" => '£',
        "euro" => '€',
        "copy" => '©',
        _ => return None,
    })
}

fn strip_apostrophe_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = 0usize;
    for c in s.chars() {
        if c == '\'' {
            run += 1;
            continue;
        }
        if run == 1 {
            out.push('\'');
        } else if run == 4 {
            out.push('\''); // '''' renders one apostrophe plus bold
        }
        run = 0;
        out.push(c);
    }
    if run == 1 {
        out.push('\'');
    }
    out
}

fn strip_magic_words(s: &str) -> String {
    let mut out = s.to_owned();
    for w in ["__NOTOC__", "__TOC__", "__FORCETOC__", "__NOEDITSECTION__", "__NOINDEX__", "__INDEX__", "__STATICREDIRECT__", "__NEWSECTIONLINK__", "__NONEWSECTIONLINK__", "__HIDDENCAT__", "__EXPECTUNUSEDCATEGORY__"] {
        out = out.replace(w, "");
    }
    out
}

/// Render page wikitext into sections of classified paragraphs.
pub fn render(wikitext: &str, screen: Screen) -> Vec<Section> {
    let mut s = strip_comments(wikitext);
    for (name, silent) in [
        ("ref", true),
        ("references", true),
        ("math", false),
        ("chem", false),
        ("ce", false),
        ("score", false),
        ("timeline", false),
        ("graph", false),
        ("hiero", false),
        ("syntaxhighlight", false),
        ("source", false),
        ("pre", false),
        ("gallery", false),
        ("imagemap", false),
        ("mapframe", false),
        ("templatedata", false),
        ("table", false),
        ("blockquote", false),
        ("poem", false),
        ("del", false),
        ("s", false),
        ("strike", false),
        ("sup", false),
        ("sub", false),
    ] {
        s = strip_element(&s, name, silent);
    }
    s = strip_templates(&s);
    s = render_links(&s);
    s = strip_tags(&s);
    s = strip_magic_words(&s);
    s = strip_apostrophe_runs(&s);
    s = decode_entities(&s);
    // Every removed block becomes its own line, so text that follows it
    // (for example after a multi-line infobox) starts a new paragraph.
    let s = s.replace(BLOCK, "\n\u{E001}\n");

    let mut sections = vec![Section { heading: None, level: 0, paras: Vec::new() }];
    let mut cur: Vec<&str> = Vec::new();
    fn flush(cur: &mut Vec<&str>, sec: &mut Section, screen: Screen) {
        if !cur.is_empty() {
            let t = squash(&cur.join(" "));
            if !t.is_empty() {
                sec.paras.push(Para::classify(t, screen));
            }
            cur.clear();
        }
    }
    for line in s.lines() {
        let t = line.trim();
        let sec = sections.last_mut().expect("section");
        if t.is_empty() {
            flush(&mut cur, sec, screen);
        } else if t.len() >= 2 && t.starts_with('=') && t.ends_with('=') {
            flush(&mut cur, sec, screen);
            let level = t.chars().take_while(|&c| c == '=').count();
            let heading = squash(t.trim_matches('='));
            sections.push(Section { heading: Some(heading), level, paras: Vec::new() });
        } else if t.starts_with(['*', '#', ':', ';']) {
            flush(&mut cur, sec, screen);
            sec.paras.push(Para::non_prose(t, "list_or_indent"));
        } else if t.starts_with("----") || t.chars().all(|c| c == BLOCK || c.is_whitespace()) {
            flush(&mut cur, sec, screen);
            sec.paras.push(Para::non_prose(String::new(), "removed_block"));
        } else {
            cur.push(line);
        }
    }
    let sec = sections.last_mut().expect("section");
    flush(&mut cur, sec, screen);
    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAD: &str = "{{Short description|Synthetic town}}\n{{Infobox settlement\n| name = Testford\n| population = {{formatnum:1200}}\n}}\n[[File:Testford.jpg|thumb|The [[market]] square]]\n'''Testford''' is a small [[market town|town]] in the [[Synthetic County]], known for its annual fair.<ref>{{cite web|title=x}}</ref> The town grew around a river crossing and later a [[railway]] station.{{cn}}\n\nIts economy relies on farming and on [http://example.org visiting traders] who arrive each summer.\n\n== History ==\nThe first settlement dates from the twelfth century.\n";

    #[test]
    fn renders_lead_links_and_silent_markers() {
        let s = render(LEAD, Screen::MARKUP);
        assert_eq!(s.len(), 2);
        let prose: Vec<_> = s[0].paras.iter().filter(|p| p.prose).map(|p| p.text.as_str()).collect();
        assert_eq!(prose, vec![
            "Testford is a small town in the Synthetic County, known for its annual fair. The town grew around a river crossing and later a railway station.",
            "Its economy relies on farming and on visiting traders who arrive each summer.",
        ]);
        assert!(s[0].paras.iter().any(|p| p.reason == Some("removed_block")));
        assert_eq!(s[1].heading.as_deref(), Some("History"));
    }

    #[test]
    fn text_after_multiline_infobox_starts_a_paragraph() {
        let s = render("{{Infobox person\n| name = X\n}}'''Jane Roe''' was a synthetic example used only in these unit tests of the renderer.", Screen::MARKUP);
        let p = s[0].paras.iter().find(|p| p.prose).expect("prose");
        assert!(p.text.starts_with("Jane Roe was"), "{p:?}");
        let s = render("The town {{multi\n|line}} grew quickly and had many people living in it by then.", Screen::MARKUP);
        assert!(s[0].paras.iter().all(|p| !p.prose), "{:?}", s[0].paras);
    }

    #[test]
    fn inline_templates_and_elements_leave_gaps() {
        let s = render("Paris ({{IPA-fr|paʁi}}) is the capital and largest city of France by population today.", Screen::MARKUP);
        assert_eq!(s[0].paras[0].reason, Some("removed_inline_content"));
        let s = render("The ratio <math>a/b</math> is constant in every case we have observed so far here.", Screen::MARKUP);
        assert_eq!(s[0].paras[0].reason, Some("removed_inline_content"));
        let s = render("An area of 5 km<sup>2</sup> is covered by the old forest and its many small lakes.", Screen::MARKUP);
        assert_eq!(s[0].paras[0].reason, Some("removed_inline_content"));
        let s = render("Unclosed {{template that never ends and the rest of the page text continues.", Screen::MARKUP);
        assert!(!s[0].paras[0].prose);
    }

    #[test]
    fn lists_tables_quotes_and_entities() {
        let s = render("* item one\n{|\n| a || b\n|}\nA plain sentence with an &amp; sign and&nbsp;a space in the middle of it.\n", Screen::MARKUP);
        let reasons: Vec<_> = s[0].paras.iter().map(|p| p.reason).collect();
        assert_eq!(reasons[0], Some("list_or_indent"));
        assert!(reasons.contains(&Some("removed_block")));
        let last = s[0].paras.last().unwrap();
        assert!(last.prose, "{last:?}");
        assert_eq!(last.text, "A plain sentence with an & sign and a space in the middle of it.");
        assert_eq!(strip_apostrophe_runs("''It's'' '''bold'''"), "It's bold");
        let s = render("She said \"we will build a new bridge over the river next year\" at the meeting today.", Screen::MARKUP);
        assert_eq!(s[0].paras[0].reason, Some("long_quotation"));
    }
}

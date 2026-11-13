//! The strip pass: the page body, decoded to UTF-8, streams through lol_html,
//! which drops the parts that are never article text and reads the page's
//! metadata and robots directives on the way, into at most `bound` bytes of
//! stripped HTML (risk F11). Then [`shape`] counts the stripped page's
//! elements and nesting depth before any DOM exists (risk F13).
//!
//! - **Charset** (risk F10): a byte-order mark, else the `Content-Type`
//!   charset, else a `<meta>` charset in the first 1,024 bytes (the HTML
//!   standard's prescan window), else UTF-8 when those bytes are valid UTF-8
//!   and windows-1252 otherwise. UTF-16 named by a `<meta>` means UTF-8, as
//!   the standard says. Decoding uses encoding_rs.
//! - **Dropped:** `script` (except JSON-LD, read for the paywall marker),
//!   `style`, `noscript`, `template`, `svg`, `math`, `iframe`, `object`,
//!   `embed`, `nav`, `header`, `footer`, `aside`, form controls, media and
//!   images, and every comment. `form` itself stays: some sites wrap the whole
//!   page in one.
//! - **Read:** `<title>`, `og:title`, `og:site_name`, `author` and
//!   `article:author`, `article:published_time`, `<link rel=canonical>`,
//!   `<html lang>`, the robots meta tags (`robots` and `SlopNinjaBot`:
//!   `noindex`, `none`, `nosnippet`, `max-snippet:0`, `noarchive`, `noai`,
//!   `noimageai`), the TDM reservation (`tdm-reservation` = 1), a password
//!   field, and JSON-LD's `"isAccessibleForFree": false`.

use crate::bound::{LD_JSON_BYTES, META_BYTES, PRESCAN_BYTES};
use encoding_rs::{CoderResult, Decoder, Encoding, UTF_8, WINDOWS_1252};
use lol_html::html_content::{Element, TextChunk};
use lol_html::{HtmlRewriter, MemorySettings, Settings, doc_comments, element, text};
use std::cell::RefCell;
use std::rc::Rc;

/// Robots directives that apply to SlopNinjaBot (from `X-Robots-Tag` or
/// the robots meta tags).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Directives {
    pub noindex: bool,
    pub nosnippet: bool,
    pub noarchive: bool,
    /// `noai` or `noimageai`.
    pub noai: bool,
}

impl Directives {
    /// No public page and no text kept.
    pub fn private(&self) -> bool {
        self.noindex || self.noai
    }

    pub fn merge(&mut self, o: Directives) {
        self.noindex |= o.noindex;
        self.nosnippet |= o.nosnippet;
        self.noarchive |= o.noarchive;
        self.noai |= o.noai;
    }

    fn apply(&mut self, token: &str) {
        let t = token.trim().to_ascii_lowercase();
        match t.as_str() {
            "noindex" => self.noindex = true,
            "none" => {
                self.noindex = true;
            }
            "nosnippet" | "max-snippet:0" => self.nosnippet = true,
            "noarchive" => self.noarchive = true,
            "noai" | "noimageai" => self.noai = true,
            _ => {}
        }
    }

    /// A robots meta tag's `content` (comma or space separated).
    pub fn apply_meta(&mut self, content: &str) {
        for t in content.split(|c: char| c == ',' || c.is_whitespace()) {
            self.apply(t);
        }
    }

    /// One `X-Robots-Tag` value. A leading `<agent>:` scopes the directives
    /// that follow it to that agent until the next one; only unscoped ones
    /// and those scoped to `product` apply.
    pub fn apply_header(&mut self, value: &str, product: &str) {
        const VALUED: &[&str] = &["max-snippet", "max-image-preview", "max-video-preview", "unavailable_after", "indexifembedded"];
        let mut applies = true;
        for raw in value.split(',') {
            let mut token = raw.trim();
            if let Some((head, rest)) = token.split_once(':') {
                let head = head.trim();
                let agent = !head.is_empty()
                    && head.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && !VALUED.iter().any(|v| v.eq_ignore_ascii_case(head));
                if agent {
                    applies = head.eq_ignore_ascii_case(product);
                    token = rest.trim();
                }
            }
            if applies {
                for t in token.split_whitespace() {
                    self.apply(t);
                }
            }
        }
    }
}

/// What the strip pass read from the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMeta {
    pub title: Option<String>,
    pub og_title: Option<String>,
    pub site_name: Option<String>,
    pub author: Option<String>,
    pub published: Option<String>,
    pub lang: Option<String>,
    /// The first `<link rel=canonical>` href, as written.
    pub canonical: Option<String>,
    pub robots: Directives,
    pub tdm_reservation: bool,
    pub password_field: bool,
    pub paywalled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripError {
    /// The stripped HTML (or lol_html's buffer) exceeds the bound.
    TooLarge,
    TooDeep,
    TooManyElements,
    /// lol_html refused the input.
    Parse,
}

/// The stripped page.
#[derive(Debug)]
pub struct Stripped {
    pub html: String,
    pub meta: PageMeta,
    /// Decoded body bytes read.
    pub body_bytes: u64,
}

#[derive(Default)]
struct State {
    out: Vec<u8>,
    bound: usize,
    overflow: bool,
    meta: PageMeta,
    title: String,
    in_title: bool,
    ld: String,
}

fn push_bounded(dst: &mut String, s: &str, cap: usize) {
    for c in s.chars() {
        if dst.len() + c.len_utf8() > cap {
            break;
        }
        dst.push(c);
    }
}

fn bounded(s: &str) -> String {
    let mut out = String::new();
    push_bounded(&mut out, s.trim(), META_BYTES);
    out
}

/// Elements removed with their content.
const DROPPED: &str = "style, noscript, template, svg, math, iframe, object, embed, nav, header, footer, aside, \
                       button, select, textarea, input, canvas, video, audio, img, picture, source, map";

type Sink = Box<dyn FnMut(&[u8])>;

/// The streaming strip pass over one body.
pub struct Stripper {
    state: Rc<RefCell<State>>,
    rewriter: Option<HtmlRewriter<'static, Sink>>,
    header_charset: Option<&'static Encoding>,
    prescan: Vec<u8>,
    decoder: Option<Decoder>,
    scratch: Vec<u8>,
    body_bytes: u64,
    failed: Option<StripError>,
}

impl Stripper {
    /// A pass holding at most `bound` bytes of stripped HTML; `charset` is
    /// the `Content-Type` parameter, if any.
    pub fn new(bound: usize, charset: Option<&str>) -> Stripper {
        let state = Rc::new(RefCell::new(State { bound, ..State::default() }));
        let s = |st: &Rc<RefCell<State>>| Rc::clone(st);
        let (s1, s2, s3, s4, s5, s6, s7, s8) = (s(&state), s(&state), s(&state), s(&state), s(&state), s(&state), s(&state), s(&state));
        let settings = Settings::new()
            .with_memory_settings(MemorySettings::new().with_max_allowed_memory_usage(bound.max(PRESCAN_BYTES)))
            .append_document_content_handler(doc_comments!(|c| {
                c.remove();
                Ok(())
            }))
            .append_element_content_handler(element!("script", move |el: &mut Element| {
                let ld = el.get_attribute("type").is_some_and(|t| t.trim().eq_ignore_ascii_case("application/ld+json"));
                if !ld {
                    el.remove();
                }
                Ok(())
            }))
            .append_element_content_handler(text!(r#"script[type="application/ld+json"]"#, move |t: &mut TextChunk| {
                let mut st = s1.borrow_mut();
                let room = LD_JSON_BYTES.saturating_sub(st.ld.len());
                if room > 0 {
                    let chunk = t.as_str().to_owned();
                    push_bounded(&mut st.ld, &chunk, LD_JSON_BYTES);
                }
                t.remove();
                Ok(())
            }))
            .append_element_content_handler(element!("input", move |el: &mut Element| {
                if el.get_attribute("type").is_some_and(|t| t.trim().eq_ignore_ascii_case("password")) {
                    s2.borrow_mut().meta.password_field = true;
                }
                Ok(())
            }))
            .append_element_content_handler(element!(DROPPED, |el: &mut Element| {
                el.remove();
                Ok(())
            }))
            .append_element_content_handler(element!("meta", move |el: &mut Element| {
                meta_tag(&mut s3.borrow_mut().meta, el);
                Ok(())
            }))
            .append_element_content_handler(element!("link", move |el: &mut Element| {
                let canonical =
                    el.get_attribute("rel").is_some_and(|r| r.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("canonical")));
                if canonical && let Some(href) = el.get_attribute("href") {
                    let mut st = s4.borrow_mut();
                    if st.meta.canonical.is_none() {
                        st.meta.canonical = Some(bounded(&href));
                    }
                }
                Ok(())
            }))
            .append_element_content_handler(element!("html", move |el: &mut Element| {
                if let Some(l) = el.get_attribute("lang") {
                    let mut st = s5.borrow_mut();
                    if st.meta.lang.is_none() && !l.trim().is_empty() {
                        st.meta.lang = Some(bounded(&l));
                    }
                }
                Ok(())
            }))
            .append_element_content_handler(element!("title", move |_el: &mut Element| {
                s6.borrow_mut().in_title = true;
                Ok(())
            }))
            .append_element_content_handler(text!("title", move |t: &mut TextChunk| {
                let mut st = s7.borrow_mut();
                if st.in_title && st.meta.title.is_none() {
                    let chunk = t.as_str().to_owned();
                    push_bounded(&mut st.title, &chunk, META_BYTES);
                    if t.last_in_text_node() {
                        let title = st.title.split_whitespace().collect::<Vec<_>>().join(" ");
                        st.meta.title = (!title.is_empty()).then_some(title);
                        st.in_title = false;
                    }
                }
                Ok(())
            }));
        let sink: Sink = Box::new(move |c: &[u8]| {
            let mut st = s8.borrow_mut();
            if st.overflow || st.out.len() + c.len() > st.bound {
                st.overflow = true;
                return;
            }
            st.out.extend_from_slice(c);
        });
        let rewriter = HtmlRewriter::new(settings, sink);
        let header_charset = charset.and_then(|c| Encoding::for_label(c.trim().trim_matches('"').as_bytes()));
        Stripper {
            state,
            rewriter: Some(rewriter),
            header_charset,
            prescan: Vec::new(),
            decoder: None,
            scratch: vec![0u8; 4096],
            body_bytes: 0,
            failed: None,
        }
    }

    /// Feeds one window of the body; `false` stops the read (the result is
    /// then an error from [`Stripper::finish`]).
    pub fn write(&mut self, chunk: &[u8]) -> bool {
        if self.failed.is_some() {
            return false;
        }
        self.body_bytes = self.body_bytes.saturating_add(chunk.len() as u64);
        if self.decoder.is_none() {
            self.prescan.extend_from_slice(chunk);
            if self.prescan.len() < PRESCAN_BYTES {
                return true;
            }
            let head = std::mem::take(&mut self.prescan);
            self.start(&head);
            return self.decode(&head, false);
        }
        self.decode(chunk, false)
    }

    fn start(&mut self, head: &[u8]) {
        let enc = choose_encoding(self.header_charset, head);
        self.decoder = Some(enc.new_decoder());
    }

    fn decode(&mut self, mut input: &[u8], last: bool) -> bool {
        let (Some(dec), Some(rw)) = (self.decoder.as_mut(), self.rewriter.as_mut()) else { return false };
        loop {
            let (result, read, written, _) = dec.decode_to_utf8(input, &mut self.scratch, last);
            if let Some(out) = self.scratch.get(..written)
                && rw.write(out).is_err()
            {
                // lol_html refuses only past its memory bound (the handlers never fail).
                self.failed = Some(StripError::TooLarge);
                return false;
            }
            input = input.get(read..).unwrap_or(&[]);
            if self.state.borrow().overflow {
                self.failed = Some(StripError::TooLarge);
                return false;
            }
            if result == CoderResult::InputEmpty {
                return true;
            }
        }
    }

    /// Ends the pass.
    pub fn finish(mut self) -> Result<Stripped, StripError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.decoder.is_none() {
            let head = std::mem::take(&mut self.prescan);
            self.start(&head);
            if !self.decode(&head, false) {
                return Err(self.failed.unwrap_or(StripError::Parse));
            }
        }
        if !self.decode(&[], true) {
            return Err(self.failed.unwrap_or(StripError::Parse));
        }
        if let Some(rw) = self.rewriter.take()
            && rw.end().is_err()
        {
            return Err(StripError::TooLarge);
        }
        let st = std::mem::take(&mut *self.state.borrow_mut());
        if st.overflow {
            return Err(StripError::TooLarge);
        }
        let mut meta = st.meta;
        meta.paywalled = paywall_marker(&st.ld);
        let html = match String::from_utf8(st.out) {
            Ok(s) => s,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        Ok(Stripped { html, meta, body_bytes: self.body_bytes })
    }
}

fn meta_tag(meta: &mut PageMeta, el: &Element) {
    let name = el.get_attribute("name").map(|n| n.trim().to_ascii_lowercase());
    let property = el.get_attribute("property").map(|n| n.trim().to_ascii_lowercase());
    let Some(content) = el.get_attribute("content") else { return };
    match name.as_deref() {
        Some("robots") => meta.robots.apply_meta(&content),
        Some(n) if n.eq_ignore_ascii_case(slop_ninja_detect_fetch_wire::PRODUCT_TOKEN) => meta.robots.apply_meta(&content),
        Some("tdm-reservation") => meta.tdm_reservation |= content.trim() == "1",
        Some("author") if meta.author.is_none() => meta.author = Some(bounded(&content)),
        _ => {}
    }
    let slot = match property.as_deref() {
        Some("og:title") => &mut meta.og_title,
        Some("og:site_name") => &mut meta.site_name,
        Some("article:published_time") => &mut meta.published,
        Some("article:author") => &mut meta.author,
        _ => return,
    };
    if slot.is_none() && !content.trim().is_empty() {
        *slot = Some(bounded(&content));
    }
}

/// JSON-LD's `"isAccessibleForFree": false` (or `"False"`, `"false"`).
fn paywall_marker(ld: &str) -> bool {
    let mut rest = ld;
    while let Some(i) = rest.find("isAccessibleForFree") {
        rest = rest.get(i + "isAccessibleForFree".len()..).unwrap_or("");
        let after = rest.trim_start_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
        if let Some(v) = after.strip_prefix(':') {
            let v = v.trim_start().trim_start_matches('"');
            if v.get(..5).is_some_and(|w| w.eq_ignore_ascii_case("false")) {
                return true;
            }
        }
    }
    false
}

/// The encoding to decode a body with: BOM, header charset, `<meta>`
/// prescan, then UTF-8 if the prescan window is valid UTF-8, else
/// windows-1252.
pub fn choose_encoding(header: Option<&'static Encoding>, head: &[u8]) -> &'static Encoding {
    if let Some((enc, _)) = Encoding::for_bom(head) {
        return enc;
    }
    if let Some(enc) = header {
        return enc;
    }
    if let Some(enc) = meta_charset(head) {
        return if enc == encoding_rs::UTF_16LE || enc == encoding_rs::UTF_16BE {
            UTF_8
        } else if enc == encoding_rs::X_USER_DEFINED {
            WINDOWS_1252
        } else {
            enc
        };
    }
    match std::str::from_utf8(head) {
        Ok(_) => UTF_8,
        // A multi-byte sequence cut by the window's end is still UTF-8.
        Err(e) if e.error_len().is_none() => UTF_8,
        Err(_) => WINDOWS_1252,
    }
}

/// A `<meta charset=...>` or `<meta http-equiv=content-type content="...;
/// charset=...">` in the prescan window.
fn meta_charset(head: &[u8]) -> Option<&'static Encoding> {
    let lower: Vec<u8> = head.iter().take(PRESCAN_BYTES).map(u8::to_ascii_lowercase).collect();
    let mut from = 0;
    while let Some(i) = find(lower.get(from..)?, b"<meta") {
        let start = from + i;
        let end = lower.get(start..).and_then(|s| s.iter().position(|&b| b == b'>')).map_or(lower.len(), |e| start + e);
        let tag = lower.get(start..end)?;
        if let Some(c) = find(tag, b"charset") {
            let v = tag.get(c + 7..)?;
            let v = v.iter().skip_while(|b| b.is_ascii_whitespace()).copied().collect::<Vec<u8>>();
            if let Some(v) = v.strip_prefix(b"=") {
                let v: Vec<u8> = v.iter().skip_while(|b| b.is_ascii_whitespace() || **b == b'"' || **b == b'\'').copied().collect();
                let label: Vec<u8> = v.iter().take_while(|b| !(b.is_ascii_whitespace() || b"\"';/>".contains(b))).copied().collect();
                if let Some(enc) = Encoding::for_label(&label) {
                    return Some(enc);
                }
            }
        }
        from = end;
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The decoded text of a plain-text body (no `<meta>` prescan).
pub fn decode_plain(bytes: &[u8], charset: Option<&str>) -> String {
    let header = charset.and_then(|c| Encoding::for_label(c.trim().trim_matches('"').as_bytes()));
    let enc = match Encoding::for_bom(bytes) {
        Some((e, _)) => e,
        None => header.unwrap_or(if std::str::from_utf8(bytes).is_ok() { UTF_8 } else { WINDOWS_1252 }),
    };
    enc.decode_with_bom_removal(bytes).0.into_owned()
}

/// The stripped page's shape, counted before any DOM exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub elements: usize,
    /// The deepest nesting reached (a bound on the DOM's, see [`shape`]).
    pub depth: usize,
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "image", "input", "keygen", "link", "meta", "param", "source", "track", "wbr",
    "frame",
];
const RAW_TEXT: &[&str] = &["title", "textarea", "style", "xmp", "iframe", "noembed", "noframes", "script", "noscript"];
const FORMATTING: &[&str] = &["a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u"];
/// Start tags that close an open `p` (the HTML tree builder's "close a p
/// element"), approximated as any open `p`.
const P_CLOSERS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "search",
    "section",
    "summary",
    "ul",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "pre",
    "listing",
    "form",
    "plaintext",
    "table",
    "hr",
    "xmp",
];
/// Elements that bound the closing of a list item, a definition, a row or a
/// cell (their scope).
const SCOPES: &[&str] = &["table", "ul", "ol", "dl", "select", "div"];

fn is(name: &str, list: &[&str]) -> bool {
    list.iter().any(|k| k.eq_ignore_ascii_case(name))
}

/// Counts elements and estimates the DOM's nesting depth of `html`, refusing
/// past `depth_bound` or `element_bound`. It tokenizes tags as the HTML
/// tokenizer does (attribute quoting, comments, raw-text elements) and
/// approximates the tree builder: void elements never nest, an end tag pops
/// to its open element (names compared exactly, case-insensitively), `p`,
/// `li`, `dt`/`dd`, table sections, rows and cells, `option`, `a`, `button`
/// and headings close their open predecessor, and formatting elements popped
/// by another end tag are still counted, since the tree builder reopens
/// them. Every approximation errs deeper, so the estimate is at least the
/// DOM's depth. Nothing is allocated but the open-element stack, which holds
/// slices of `html`.
pub fn shape(html: &str, depth_bound: usize, element_bound: usize) -> Result<Shape, StripError> {
    let b = html.as_bytes();
    let mut stack: Vec<&str> = Vec::new();
    let mut pending_fmt: usize = 0;
    let mut elements = 0usize;
    let mut depth = 0usize;
    let mut i = 0usize;
    while let Some(rel) = b.get(i..).and_then(|s| s.iter().position(|&c| c == b'<')) {
        i += rel + 1;
        match b.get(i) {
            Some(b'!') => {
                if b.get(i + 1..i + 3) == Some(b"--") {
                    let body = i + 3;
                    if b.get(body) == Some(&b'>') {
                        i = body + 1;
                    } else if b.get(body..body + 2) == Some(b"->") {
                        i = body + 2;
                    } else {
                        match html.get(body..).and_then(|s| s.find("-->")) {
                            Some(e) => i = body + e + 3,
                            None => break,
                        }
                    }
                } else {
                    i = skip_to_gt(b, i);
                }
            }
            Some(b'?') => i = skip_to_gt(b, i),
            Some(b'/') => {
                let n = i + 1;
                if b.get(n).is_some_and(u8::is_ascii_alphabetic) {
                    let (name, after) = tag_name(html, n);
                    let Some((end, _)) = tag_end(b, after) else { break };
                    i = end;
                    end_tag(&mut stack, &mut pending_fmt, name);
                } else {
                    i = skip_to_gt(b, n);
                }
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let (name, after) = tag_name(html, i);
                let Some((end, _self_closing)) = tag_end(b, after) else { break };
                i = end;
                elements += 1;
                if elements > element_bound {
                    return Err(StripError::TooManyElements);
                }
                if is(name, &["html", "head", "body"]) {
                    continue;
                }
                start_tag(&mut stack, &mut pending_fmt, name);
                let d = stack.len() + pending_fmt + 2;
                depth = depth.max(d);
                if d > depth_bound {
                    return Err(StripError::TooDeep);
                }
                if name.eq_ignore_ascii_case("plaintext") {
                    break;
                }
                if is(name, RAW_TEXT) {
                    match find_end_tag(b, i, name) {
                        Some(p) => i = p,
                        None => break,
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Shape { elements, depth })
}

/// The index of the next `</name` (case-insensitive) followed by
/// whitespace, `/` or `>`, from `from`.
fn find_end_tag(b: &[u8], from: usize, name: &str) -> Option<usize> {
    let n = name.as_bytes();
    let mut i = from;
    while let Some(rel) = b.get(i..).and_then(|s| s.windows(2).position(|w| w == b"</")) {
        let at = i + rel;
        let start = at + 2;
        if b.get(start..start + n.len()).is_some_and(|w| w.eq_ignore_ascii_case(n))
            && b.get(start + n.len()).is_none_or(|c| c.is_ascii_whitespace() || *c == b'/' || *c == b'>')
        {
            return Some(at);
        }
        i = at + 2;
    }
    None
}

fn skip_to_gt(b: &[u8], from: usize) -> usize {
    b.get(from..).and_then(|s| s.iter().position(|&c| c == b'>')).map_or(b.len(), |p| from + p + 1)
}

/// A tag name from `start` (as written) and the index after it.
fn tag_name(html: &str, start: usize) -> (&str, usize) {
    let b = html.as_bytes();
    let mut j = start;
    while b.get(j).is_some_and(|c| !(c.is_ascii_whitespace() || *c == b'/' || *c == b'>')) {
        j += 1;
    }
    (html.get(start..j).unwrap_or(""), j)
}

/// Runs the tokenizer's attribute states from `from` to the tag's `>`:
/// the index after it and the self-closing flag; `None` at the end of
/// input (the tokenizer then emits nothing).
fn tag_end(b: &[u8], from: usize) -> Option<(usize, bool)> {
    #[derive(Clone, Copy)]
    enum S {
        BeforeName,
        Name,
        AfterName,
        BeforeValue,
        Dq,
        Sq,
        Unquoted,
        AfterQuoted,
        SelfClosing,
    }
    let mut s = S::BeforeName;
    let mut i = from;
    loop {
        let c = *b.get(i)?;
        let ws = c.is_ascii_whitespace();
        let mut advance = true;
        s = match s {
            S::BeforeName if ws => S::BeforeName,
            S::BeforeName if c == b'/' => S::SelfClosing,
            S::BeforeName if c == b'>' => return Some((i + 1, false)),
            S::BeforeName => S::Name,
            S::Name if ws => S::AfterName,
            S::Name if c == b'/' => S::SelfClosing,
            S::Name if c == b'>' => return Some((i + 1, false)),
            S::Name if c == b'=' => S::BeforeValue,
            S::Name => S::Name,
            S::AfterName if ws => S::AfterName,
            S::AfterName if c == b'/' => S::SelfClosing,
            S::AfterName if c == b'=' => S::BeforeValue,
            S::AfterName if c == b'>' => return Some((i + 1, false)),
            S::AfterName => S::Name,
            S::BeforeValue if ws => S::BeforeValue,
            S::BeforeValue if c == b'"' => S::Dq,
            S::BeforeValue if c == b'\'' => S::Sq,
            S::BeforeValue if c == b'>' => return Some((i + 1, false)),
            S::BeforeValue => S::Unquoted,
            S::Dq if c == b'"' => S::AfterQuoted,
            S::Dq => S::Dq,
            S::Sq if c == b'\'' => S::AfterQuoted,
            S::Sq => S::Sq,
            S::Unquoted if ws => S::BeforeName,
            S::Unquoted if c == b'>' => return Some((i + 1, false)),
            S::Unquoted => S::Unquoted,
            S::AfterQuoted if ws => S::BeforeName,
            S::AfterQuoted if c == b'/' => S::SelfClosing,
            S::AfterQuoted if c == b'>' => return Some((i + 1, false)),
            S::AfterQuoted => {
                advance = false;
                S::BeforeName
            }
            S::SelfClosing if c == b'>' => return Some((i + 1, true)),
            S::SelfClosing => {
                advance = false;
                S::BeforeName
            }
        };
        if advance {
            i += 1;
        }
    }
}

fn last_pos(stack: &[&str], names: &[&str]) -> Option<usize> {
    stack.iter().rposition(|n| is(n, names))
}

/// Pops to `at` (exclusive of what stays below it); formatting elements
/// popped other than `keep` count as pending, since the tree builder
/// reopens them.
fn pop_to(stack: &mut Vec<&str>, pending_fmt: &mut usize, at: usize, keep: Option<&str>) {
    while stack.len() > at {
        if let Some(n) = stack.pop()
            && is(n, FORMATTING)
            && !keep.is_some_and(|k| k.eq_ignore_ascii_case(n))
        {
            *pending_fmt += 1;
        }
    }
}

fn start_tag<'h>(stack: &mut Vec<&'h str>, pending_fmt: &mut usize, name: &'h str) {
    if is(name, P_CLOSERS)
        && let Some(p) = last_pos(stack, &["p"])
    {
        pop_to(stack, pending_fmt, p, None);
    }
    let lower = |n: &str| -> &'static [&'static str] {
        const H: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
        if is(n, &["li"]) {
            &["li"]
        } else if is(n, &["dt", "dd"]) {
            &["dt", "dd"]
        } else if is(n, &["option"]) {
            &["option"]
        } else if is(n, &["optgroup"]) {
            &["option", "optgroup"]
        } else if is(n, &["tr"]) {
            &["tr"]
        } else if is(n, &["td", "th"]) {
            &["td", "th"]
        } else if is(n, &["thead", "tbody", "tfoot"]) {
            &["thead", "tbody", "tfoot"]
        } else if is(n, H) {
            H
        } else if is(n, &["a"]) {
            &["a"]
        } else if is(n, &["button"]) {
            &["button"]
        } else if is(n, &["nobr"]) {
            &["nobr"]
        } else {
            &[]
        }
    };
    let closes = lower(name);
    if !closes.is_empty()
        && let Some(at) = last_pos(stack, closes)
    {
        let heading = name.len() == 2 && is(name, &["h1", "h2", "h3", "h4", "h5", "h6"]);
        let scope = last_pos(stack, SCOPES);
        if (heading && at + 1 == stack.len()) || (!heading && scope.is_none_or(|s| at > s)) {
            pop_to(stack, pending_fmt, at, None);
        }
    }
    if !is(name, VOID) {
        stack.push(name);
    }
}

fn end_tag(stack: &mut Vec<&str>, pending_fmt: &mut usize, name: &str) {
    match last_pos(stack, &[name]) {
        Some(at) => pop_to(stack, pending_fmt, at, Some(name)),
        None => {
            if is(name, FORMATTING) && *pending_fmt > 0 {
                *pending_fmt -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(html: &str, bound: usize) -> Result<Stripped, StripError> {
        let mut s = Stripper::new(bound, None);
        for w in html.as_bytes().chunks(5) {
            if !s.write(w) {
                break;
            }
        }
        s.finish()
    }

    #[test]
    fn drops_non_text_parts_and_reads_metadata() {
        let html = r#"<!doctype html><html lang="en-GB"><head><title>  The   Title </title>
            <meta property="og:title" content="OG Title"><meta property="og:site_name" content="Site">
            <meta name="author" content="A. Writer"><meta property="article:published_time" content="2026-01-02T03:04:05Z">
            <link rel="stylesheet" href="/s.css"><link rel="Canonical" href="https://example.com/canon">
            <style>p{color:red}</style><script>var secret = "x";</script>
            <script type="application/ld+json">{"@type":"NewsArticle","isAccessibleForFree": false}</script></head>
            <body><nav>Menu</nav><header>Head</header><!-- a comment --><article><p>Body text.</p>
            <form><input type="password" name="pw"><p>Inside form.</p></form><img src="data:x"></article>
            <aside>Related</aside><footer>Foot</footer><noscript>Enable JS</noscript><svg><text>x</text></svg></body></html>"#;
        let s = strip(html, 1 << 20).unwrap();
        assert!(s.html.contains("Body text.") && s.html.contains("Inside form."));
        for gone in ["Menu", "Head<", "a comment", "color:red", "secret", "Related", "Foot", "Enable JS", "data:x", "password"] {
            assert!(!s.html.contains(gone), "{gone} survived: {}", s.html);
        }
        assert_eq!(s.meta.title.as_deref(), Some("The Title"));
        assert_eq!(s.meta.og_title.as_deref(), Some("OG Title"));
        assert_eq!(s.meta.site_name.as_deref(), Some("Site"));
        assert_eq!(s.meta.author.as_deref(), Some("A. Writer"));
        assert_eq!(s.meta.published.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!(s.meta.lang.as_deref(), Some("en-GB"));
        assert_eq!(s.meta.canonical.as_deref(), Some("https://example.com/canon"));
        assert!(s.meta.password_field && s.meta.paywalled);
        assert_eq!(s.meta.robots, Directives::default());
        assert!(!s.meta.tdm_reservation);
        assert_eq!(s.body_bytes, html.len() as u64);
    }

    #[test]
    fn robots_meta_tags() {
        for (tag, want) in [
            (r#"<meta name="robots" content="noindex, follow">"#, Directives { noindex: true, ..Directives::default() }),
            (r#"<meta name="ROBOTS" content="none">"#, Directives { noindex: true, ..Directives::default() }),
            (r#"<meta name="SlopNinjaBot" content="noai">"#, Directives { noai: true, ..Directives::default() }),
            (
                r#"<meta name="robots" content="max-snippet:0 noarchive">"#,
                Directives { nosnippet: true, noarchive: true, ..Directives::default() },
            ),
            (r#"<meta name="googlebot" content="noindex">"#, Directives::default()),
        ] {
            let s = strip(&format!("<html><head>{tag}</head><body><p>x</p></body></html>"), 1 << 20).unwrap();
            assert_eq!(s.meta.robots, want, "{tag}");
        }
        let s = strip(r#"<meta name="tdm-reservation" content="1"><p>x</p>"#, 1 << 20).unwrap();
        assert!(s.meta.tdm_reservation);
    }

    #[test]
    fn x_robots_tag_scopes() {
        let mut d = Directives::default();
        d.apply_header("googlebot: noindex, nofollow", "SlopNinjaBot");
        assert_eq!(d, Directives::default());
        d.apply_header("max-snippet: 20, noarchive", "SlopNinjaBot");
        assert!(d.noarchive && !d.noindex);
        d.apply_header("otherbot: noai, SlopNinjaBot: noindex", "SlopNinjaBot");
        assert!(d.noindex && !d.noai);
        let mut e = Directives::default();
        e.apply_header("NoIndex", "SlopNinjaBot");
        assert!(e.private());
        let mut f = Directives::default();
        f.apply_header("noai", "SlopNinjaBot");
        assert!(f.private());
    }

    #[test]
    fn the_bound_refuses_and_never_truncates() {
        let p = "<p>".to_owned() + &"word ".repeat(400) + "</p>";
        assert!(strip(&p, 4096).is_ok());
        let big = p.repeat(10);
        assert_eq!(strip(&big, 4096).unwrap_err(), StripError::TooLarge);
        // Removed content does not count against the bound.
        let scripts = "<script>".to_owned() + &"x".repeat(100_000) + "</script><p>short</p>";
        assert_eq!(strip(&scripts, 4096).unwrap().html.trim(), "<p>short</p>");
        // A giant attribute is held by lol_html's buffer, bounded the same way.
        let attr = format!("<div title=\"{}\"><p>x</p></div>", "a".repeat(100_000));
        assert_eq!(strip(&attr, 4096).unwrap_err(), StripError::TooLarge);
    }

    #[test]
    fn charsets() {
        // Header charset.
        let mut s = Stripper::new(1 << 20, Some("windows-1252"));
        s.write(b"<p>caf\xe9 \x93quoted\x94</p>");
        assert!(s.finish().unwrap().html.contains("caf\u{e9} \u{201c}quoted\u{201d}"));
        // Meta charset in the prescan window.
        let html = b"<html><head><meta charset=\"iso-8859-1\"></head><body><p>na\xefve</p></body></html>";
        let mut s = Stripper::new(1 << 20, None);
        s.write(html);
        assert!(s.finish().unwrap().html.contains("na\u{ef}ve"));
        let html = b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=Shift_JIS\"><p>\x82\xa0</p>";
        let mut s = Stripper::new(1 << 20, None);
        s.write(html);
        assert!(s.finish().unwrap().html.contains('\u{3042}'));
        // BOM wins over the header.
        let mut s = Stripper::new(1 << 20, Some("windows-1252"));
        s.write(b"\xef\xbb\xbf<p>\xe2\x82\xac</p>");
        assert!(s.finish().unwrap().html.contains('\u{20ac}'));
        // No declaration: valid UTF-8 stays UTF-8, anything else is windows-1252.
        assert_eq!(choose_encoding(None, "<p>\u{20ac}</p>".as_bytes()), UTF_8);
        assert_eq!(choose_encoding(None, b"<p>\x80</p>"), WINDOWS_1252);
        // A meta naming UTF-16 means UTF-8.
        assert_eq!(choose_encoding(None, b"<meta charset=utf-16>"), UTF_8);
        assert_eq!(decode_plain(b"\xff\xfeh\x00i\x00", None), "hi");
    }

    #[test]
    fn shape_counts_depth_and_elements() {
        let s = shape("<html><body><div><p>a<p>b<p>c</div><ul><li>x<li>y</ul></body></html>", 100, 100).unwrap();
        assert_eq!(s.elements, 9);
        assert_eq!(s.depth, 4);
        let deep = "<div>".repeat(300) + &"</div>".repeat(300);
        assert_eq!(shape(&deep, 256, 10_000).unwrap_err(), StripError::TooDeep);
        assert_eq!(shape(&deep, 400, 10_000).unwrap().depth, 302);
        // Unclosed paragraphs and list items do not nest.
        let flat = "<p>x".repeat(5000);
        assert_eq!(shape(&flat, 256, 10_000).unwrap().depth, 3);
        let items = "<ul>".to_owned() + &"<li>x".repeat(5000) + "</ul>";
        assert_eq!(shape(&items, 256, 10_000).unwrap().depth, 4);
        assert_eq!(shape(&items, 256, 1000).unwrap_err(), StripError::TooManyElements);
        // Void elements never nest.
        assert_eq!(shape(&"<br><img src=x><hr>".repeat(1000), 10, 10_000).unwrap().depth, 2);
        // Unclosed formatting popped by a block end still counts.
        let fmt = "<div>".to_owned() + &"<b>".repeat(100) + "</div>" + &"<div>".repeat(10);
        assert!(shape(&fmt, 1000, 10_000).unwrap().depth >= 112);
    }

    #[test]
    fn shape_tokenizes_like_the_html_tokenizer() {
        // A quote inside an attribute name does not open a value: the divs
        // after it still count.
        let s = "<div a\"=x>".to_owned() + &"<div>".repeat(50);
        assert_eq!(shape(&s, 1000, 10_000).unwrap().depth, 53);
        // Quoted values may hold '>'.
        let s = "<div title=\"a>b\">".to_owned() + &"<div>".repeat(5);
        assert_eq!(shape(&s, 1000, 10_000).unwrap().depth, 8);
        // Comments and raw text hold no elements.
        let s = "<!-- <div><div> --><title><div><div></title><textarea><div></TEXTAREA ><div>";
        assert_eq!(shape(s, 1000, 10_000).unwrap(), Shape { elements: 3, depth: 3 });
        // Abrupt comment closings.
        assert_eq!(shape("<!--><div><!---><div>", 1000, 100).unwrap().depth, 4);
        // An end tag pops to its element.
        assert_eq!(shape("<div><span><b></div><div>", 100, 100).unwrap().depth, 5);
    }
}

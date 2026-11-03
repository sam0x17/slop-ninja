//! Minimal HTML-to-paragraph renderer for sanitised post bodies.
//!
//! Quotations, code blocks, tables, lists and headings become non-prose
//! breaks; images, line breaks, superscripts and unknown tags leave a gap so
//! their paragraph is never admitted.
use crate::{
    text::{GAP, Para, Screen, squash},
    wikitext::decode_entities,
};

pub const RENDER_METHOD: &str = "slop_ninja_post_html_paragraphs_v1";

const INLINE_KEEP: &[&str] = &["a", "em", "strong", "b", "i", "code", "kbd", "span", "abbr", "small", "u", "cite", "q", "var", "samp", "dfn"];
const DROP_CONTENT: &[(&str, &str)] = &[
    ("blockquote", "quotation"),
    ("pre", "code_block"),
    ("table", "table"),
    ("script", "script"),
    ("style", "style"),
    ("svg", "svg"),
    ("aside", "aside"),
];
const BLOCKS: &[&str] = &["p", "div", "section", "article", "body", "html", "dl", "dt", "dd", "figure", "figcaption"];
const LISTS: &[&str] = &["ul", "ol", "li"];
const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

struct State {
    paras: Vec<Para>,
    cur: String,
    list_depth: usize,
    heading: bool,
    screen: Screen,
}

impl State {
    fn flush(&mut self) {
        let t = squash(&self.cur);
        self.cur.clear();
        if t.is_empty() {
            return;
        }
        if self.heading {
            self.paras.push(Para::non_prose(t, "heading"));
        } else if self.list_depth > 0 {
            self.paras.push(Para::non_prose(t, "list_item"));
        } else {
            self.paras.push(Para::classify(t, self.screen));
        }
    }
}

pub fn render(html: &str, screen: Screen) -> Vec<Para> {
    render_with(html, screen, &[])
}

/// Value of a `class` attribute in a raw start tag, if any.
pub fn class_attr(tag: &str) -> Option<&str> {
    let i = tag.find("class=")?;
    let rest = &tag[i + 6..];
    let q = rest.chars().next()?;
    if q == '"' || q == '\'' {
        let inner = &rest[1..];
        inner.find(q).map(|j| &inner[..j])
    } else {
        rest.split([' ', '>', '/']).next()
    }
}

/// `render` with extra drop rules: an element whose `class` attribute
/// contains one of `drop_classes` is skipped whole and leaves a non-prose
/// break (for example image captions and embedded boxes in post bodies).
pub fn render_with(html: &str, screen: Screen, drop_classes: &[&str]) -> Vec<Para> {
    let mut st = State { paras: Vec::new(), cur: String::new(), list_depth: 0, heading: false, screen };
    let mut skip: Option<(String, usize)> = None;
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            if skip.is_none() {
                st.cur.push_str(&decode_entities(rest));
            }
            break;
        };
        if skip.is_none() {
            st.cur.push_str(&decode_entities(&rest[..lt]));
        }
        let tail = &rest[lt..];
        if let Some(c) = tail.strip_prefix("<!--") {
            rest = c.find("-->").map(|k| &c[k + 3..]).unwrap_or("");
            continue;
        }
        let Some(gt) = tail.find('>') else {
            if skip.is_none() {
                st.cur.push(GAP);
            }
            break;
        };
        let tag = &tail[1..gt];
        rest = &tail[gt + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if name.is_empty() {
            if skip.is_none() {
                st.cur.push('<');
                st.cur.push_str(tag);
                st.cur.push('>');
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        if let Some((n, depth)) = skip.as_mut() {
            if name == *n && !self_closing {
                if closing {
                    *depth -= 1;
                } else {
                    *depth += 1;
                }
            }
            let done = *depth == 0;
            if done {
                skip = None;
            }
            continue;
        }
        if !closing && !self_closing && !drop_classes.is_empty() {
            if let Some(cls) = class_attr(tag) {
                if cls.split_whitespace().any(|c| drop_classes.contains(&c)) {
                    st.flush();
                    st.paras.push(Para::non_prose(String::new(), "dropped_class_element"));
                    skip = Some((name, 1));
                    continue;
                }
            }
        }
        if let Some((_, reason)) = DROP_CONTENT.iter().find(|(n, _)| *n == name) {
            if !closing && !self_closing {
                st.flush();
                st.paras.push(Para::non_prose(String::new(), reason));
                skip = Some((name, 1));
            }
            continue;
        }
        if BLOCKS.contains(&name.as_str()) {
            st.flush();
        } else if LISTS.contains(&name.as_str()) {
            st.flush();
            if name != "li" {
                if closing {
                    st.list_depth = st.list_depth.saturating_sub(1);
                } else {
                    st.list_depth += 1;
                }
            }
        } else if HEADINGS.contains(&name.as_str()) {
            st.flush();
            st.heading = !closing;
        } else if name == "hr" {
            st.flush();
            st.paras.push(Para::non_prose(String::new(), "rule"));
        } else if !INLINE_KEEP.contains(&name.as_str()) {
            // img, br, sup, sub, del, s, strike, unknown: content would not
            // render as plain contiguous prose.
            st.cur.push(GAP);
        }
    }
    st.flush();
    st.paras
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_paragraphs_and_breaks() {
        let html = "<p>I tried the <em>usual</em> approach with <a href=\"x\">this link</a> and it worked well for me in the end.</p>\n<blockquote><p>Someone else wrote this quoted sentence about the problem we discussed.</p></blockquote><ul><li>An item that looks like a sentence but is in a list.</li></ul><pre><code>x = 1</code></pre><p>Second paragraph with an &amp; and &quot;quotes&quot; written by the same author.</p><p>Has an image <img src=\"y\"> inside it which leaves a visible gap in the text here.</p>";
        let p = render(html, Screen::XML);
        let summary: Vec<_> = p.iter().map(|p| (p.prose, p.reason)).collect();
        assert_eq!(summary, vec![
            (true, None),
            (false, Some("quotation")),
            (false, Some("list_item")),
            (false, Some("code_block")),
            (true, None),
            (false, Some("removed_inline_content")),
        ]);
        assert_eq!(p[0].text, "I tried the usual approach with this link and it worked well for me in the end.");
        assert!(p[4].text.contains("& and \"quotes\""));
    }
    #[test]
    fn dropped_classes_are_skipped_whole() {
        let html = "<div id=\"a\" class=\"wp-caption aligncenter\"><img src=\"x\"><p class=\"wp-caption-text\">A caption that is a full sentence about the photo.</p></div><p>The article's own first paragraph is written as ordinary prose here.</p>";
        let p = render_with(html, Screen::MARKUP, &["wp-caption"]);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].reason, Some("dropped_class_element"));
        assert!(p[1].prose);
        assert_eq!(class_attr("p class='x y'"), Some("x y"));
        assert_eq!(render(html, Screen::MARKUP).len(), 3);
    }
    #[test]
    fn nested_quotations_are_skipped_whole() {
        let p = render("<blockquote>a<blockquote>b</blockquote>c</blockquote><p>After the nested quotation this sentence is the author's own words.</p>", Screen::XML);
        assert_eq!(p.len(), 2);
        assert!(p[1].prose);
    }
}

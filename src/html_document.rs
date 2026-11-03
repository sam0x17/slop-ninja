//! Ordered observations of an HTML fragment, before prose selection or attribution.
use anyhow::{Result, ensure};
use scraper::{Html, Node, Selector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::util::digest;

pub const VERSION: &str = "slop_ninja_html_document_v1";
const HTML_NS: &str = "http://www.w3.org/1999/xhtml";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub input_bytes: usize,
    pub nodes: usize,
    pub depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            input_bytes: 1_048_576,
            nodes: 100_000,
            depth: 256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribute {
    pub namespace: String,
    pub prefix: Option<String>,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Value {
    Document,
    Fragment,
    Element {
        namespace: String,
        prefix: Option<String>,
        name: String,
        attributes: Vec<Attribute>,
    },
    Text {
        decoded: String,
    },
    Comment {
        text: String,
    },
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    ProcessingInstruction {
        target: String,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Index in preorder of the recovered DOM, not a byte offset in the source.
    pub id: usize,
    pub parent: Option<usize>,
    pub subtree_end_exclusive: usize,
    /// Nearest enclosing observed region, excluding this node itself.
    pub region_parent: Option<usize>,
    /// Structural observations only; these do not identify an author or speaker.
    pub roles: Vec<String>,
    /// Ordinal in the pinned parser's `p` selector, for the legacy crosswalk.
    /// Use `id` for recovered DOM order; selector order can differ after recovery.
    pub paragraph_ordinal: Option<usize>,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub schema: String,
    pub parser: String,
    pub source_html: String,
    pub source_html_sha256: String,
    pub limits: Limits,
    pub parse_errors: Vec<String>,
    pub quirks_mode: String,
    pub nodes: Vec<Observation>,
    pub region_ids: Vec<usize>,
    /// Text outside an observed region remains present and needs review.
    pub unassigned_non_whitespace_text_ids: Vec<usize>,
}

/// Parse as an HTML body fragment. Preserve source bytes alongside the recovered DOM.
/// No network request, script execution, text filtering or origin classification occurs.
pub fn observe(html: &str, limits: Limits) -> Result<Document> {
    ensure!(
        html.len() <= limits.input_bytes,
        "HTML input byte limit exceeded"
    );
    let parsed = Html::parse_fragment(html);
    let raw_nodes: Vec<_> = parsed.tree.root().descendants().collect();
    ensure!(
        raw_nodes.len() <= limits.nodes,
        "Recovered DOM node limit exceeded"
    );
    let ids: HashMap<_, _> = raw_nodes
        .iter()
        .enumerate()
        .map(|(id, node)| (node.id(), id))
        .collect();
    let mut nodes: Vec<Observation> = Vec::with_capacity(raw_nodes.len());
    let mut depths = Vec::with_capacity(raw_nodes.len());
    let paragraphs: HashMap<_, _> = parsed
        .select(&Selector::parse("p").expect("static selector"))
        .enumerate()
        .map(|(ordinal, p)| (p.id(), ordinal))
        .collect();
    let mut region_ids = Vec::new();
    let mut unassigned_non_whitespace_text_ids = Vec::new();
    for (id, node) in raw_nodes.iter().enumerate() {
        let parent = node.parent().map(|p| ids[&p.id()]);
        ensure!(
            parent.is_none_or(|p| p < id),
            "Unexpected DOM traversal order"
        );
        let depth = parent.map_or(0, |p| depths[p] + 1);
        ensure!(depth <= limits.depth, "Recovered DOM depth limit exceeded");
        depths.push(depth);
        let region_parent = parent.and_then(|p| {
            if nodes[p].roles.is_empty() {
                nodes[p].region_parent
            } else {
                Some(p)
            }
        });
        let mut roles = Vec::new();
        let ordinal = paragraphs.get(&node.id()).copied();
        let value = match node.value() {
            Node::Document => Value::Document,
            Node::Fragment => Value::Fragment,
            Node::Text(text) => {
                if region_parent.is_none() && !text.trim().is_empty() {
                    unassigned_non_whitespace_text_ids.push(id);
                }
                Value::Text {
                    decoded: text.to_string(),
                }
            }
            Node::Comment(comment) => Value::Comment {
                text: comment.to_string(),
            },
            Node::Doctype(d) => Value::Doctype {
                name: d.name.to_string(),
                public_id: d.public_id.to_string(),
                system_id: d.system_id.to_string(),
            },
            Node::ProcessingInstruction(p) => Value::ProcessingInstruction {
                target: p.target.to_string(),
                data: p.data.to_string(),
            },
            Node::Element(element) => {
                if element.name.ns.as_ref() == HTML_NS {
                    if let Some(role) = tag_role(element.name()) {
                        roles.push(role.to_owned());
                    }
                    for (class, role) in [
                        ("wp-caption", "caption_container"),
                        ("wp-caption-text", "caption"),
                        ("factbox", "factbox"),
                        ("translation", "translation_class"),
                        ("inline-rss", "syndication_class"),
                        ("twitter-tweet", "embedded_post_class"),
                    ] {
                        if element.classes().any(|c| c == class) && !roles.iter().any(|r| r == role)
                        {
                            roles.push(role.to_owned());
                        }
                    }
                } else {
                    roles.push("foreign_namespace".to_owned());
                }
                let mut attributes: Vec<_> = element
                    .attrs
                    .iter()
                    .map(|(name, value)| Attribute {
                        namespace: name.ns.to_string(),
                        prefix: name.prefix.as_ref().map(ToString::to_string),
                        name: name.local.to_string(),
                        value: value.to_string(),
                    })
                    .collect();
                attributes.sort_by(|a, b| {
                    (&a.namespace, &a.name, &a.prefix).cmp(&(&b.namespace, &b.name, &b.prefix))
                });
                Value::Element {
                    namespace: element.name.ns.to_string(),
                    prefix: element.name.prefix.as_ref().map(ToString::to_string),
                    name: element.name().to_owned(),
                    attributes,
                }
            }
        };
        if !roles.is_empty() {
            region_ids.push(id);
        }
        nodes.push(Observation {
            id,
            parent,
            subtree_end_exclusive: id + 1,
            region_parent,
            roles,
            paragraph_ordinal: ordinal,
            value,
        });
    }
    for id in (0..nodes.len()).rev() {
        if let Some(parent) = nodes[id].parent {
            nodes[parent].subtree_end_exclusive = nodes[id]
                .subtree_end_exclusive
                .max(nodes[parent].subtree_end_exclusive);
        }
    }
    Ok(Document {
        schema: VERSION.to_owned(),
        parser: "scraper-0.27.0;html5ever-0.39.0;body-fragment".to_owned(),
        source_html: html.to_owned(),
        source_html_sha256: digest(html),
        limits,
        parse_errors: parsed.errors.iter().map(ToString::to_string).collect(),
        quirks_mode: format!("{:?}", parsed.quirks_mode),
        nodes,
        region_ids,
        unassigned_non_whitespace_text_ids,
    })
}

fn tag_role(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "p" => "paragraph",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "blockquote" => "blockquote",
        "q" => "inline_quote",
        "pre" => "preformatted",
        "code" => "code",
        "ul" | "ol" | "dl" => "list",
        "li" | "dt" | "dd" => "list_item",
        "figure" => "figure",
        "figcaption" | "caption" => "caption",
        "table" => "table",
        "thead" | "tbody" | "tfoot" | "tr" | "th" | "td" => "table_part",
        "iframe" | "img" | "video" | "audio" | "object" | "embed" | "source" | "track" => "media",
        "script" | "style" => "nonprose_container",
        "template" | "noscript" => "conditional_container",
        "hr" => "thematic_break",
        "div" | "section" | "article" | "aside" | "main" | "header" | "footer" | "nav" => {
            "container"
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region_text(doc: &Document, id: usize) -> String {
        doc.nodes[id..doc.nodes[id].subtree_end_exclusive]
            .iter()
            .filter_map(|n| {
                if let Value::Text { decoded } = &n.value {
                    Some(decoded.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn inline_quotes_and_unicode_preserve_surrounding_narration() {
        let html = "<p>She called it “HOME” &amp; meant <em>home</em>. <q>Really?</q> Yes.<br>é\u{a0}🙂</p>";
        let d = observe(html, Limits::default()).unwrap();
        assert_eq!(d.source_html, html);
        let p = d
            .nodes
            .iter()
            .find(|n| n.paragraph_ordinal == Some(0))
            .unwrap();
        assert_eq!(
            region_text(&d, p.id),
            "She called it “HOME” & meant home. Really? Yes.é\u{a0}🙂"
        );
        let q = d
            .nodes
            .iter()
            .find(|n| n.roles == ["inline_quote"])
            .unwrap();
        assert_eq!(q.region_parent, Some(p.id));
        assert!(
            d.nodes
                .iter()
                .any(|n| matches!(&n.value, Value::Element { name, .. } if name == "br"))
        );
    }

    #[test]
    fn quote_turns_and_nested_regions_keep_order_and_containment() {
        let d = observe("<p><b>Q:</b> Why?</p><blockquote><p>A: First.</p><p>Second.</p><blockquote><p>Nested.</p></blockquote></blockquote><p>After.</p>", Limits::default()).unwrap();
        let ps: Vec<_> = d
            .nodes
            .iter()
            .filter(|n| n.paragraph_ordinal.is_some())
            .collect();
        let quotes: Vec<_> = d
            .nodes
            .iter()
            .filter(|n| n.roles == ["blockquote"])
            .collect();
        assert_eq!(ps.len(), 5);
        assert_eq!(quotes.len(), 2);
        assert_eq!(ps[1].region_parent, Some(quotes[0].id));
        assert_eq!(ps[2].region_parent, Some(quotes[0].id));
        assert_eq!(ps[3].region_parent, Some(quotes[1].id));
        assert_eq!(quotes[1].region_parent, Some(quotes[0].id));
        assert!(ps[4].id >= quotes[0].subtree_end_exclusive);
        assert_eq!(region_text(&d, ps[4].id), "After.");
    }

    #[test]
    fn media_captions_preformatted_lists_and_unwrapped_text_survive() {
        let html = "Loose text<h2>Title</h2><div class='wp-caption'><img src='local.png'><p class='wp-caption-text'>Own notice</p></div><p><iframe src='https://invalid.example/'></iframe></p><ul><li>one</li><li>two</li></ul><pre><code>a  b\n  c</code></pre><!--note--><svg><text>diagram</text></svg><template><p>Conditional prose.</p></template>";
        let d = observe(html, Limits::default()).unwrap();
        for role in [
            "heading",
            "caption_container",
            "caption",
            "media",
            "list",
            "list_item",
            "preformatted",
            "code",
            "foreign_namespace",
        ] {
            assert!(
                d.nodes.iter().any(|n| n.roles.iter().any(|r| r == role)),
                "{role}"
            );
        }
        assert_eq!(d.unassigned_non_whitespace_text_ids.len(), 1);
        let code = d.nodes.iter().find(|n| n.roles == ["code"]).unwrap();
        assert_eq!(region_text(&d, code.id), "a  b\n  c");
        let media: Vec<_> = d.nodes.iter().filter(|n| n.roles == ["media"]).collect();
        assert_eq!(media.len(), 2);
        assert!(media.iter().all(|n| region_text(&d, n.id).is_empty()));
        assert!(
            d.nodes
                .iter()
                .any(|n| matches!(&n.value, Value::Comment { text } if text == "note"))
        );
        let template = d
            .nodes
            .iter()
            .find(|n| n.roles == ["conditional_container"])
            .unwrap();
        assert_eq!(region_text(&d, template.id), "Conditional prose.");
    }

    #[test]
    fn recovery_and_resource_limits_are_explicit() {
        let html = "<p id='one' id='two'>one<b>two</p>after</b>";
        let d = observe(html, Limits::default()).unwrap();
        assert_eq!(d.source_html, html);
        assert!(!d.parse_errors.is_empty());
        assert!(
            observe(
                html,
                Limits {
                    input_bytes: 2,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        assert!(
            observe(
                html,
                Limits {
                    nodes: 2,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        assert!(
            observe(
                "<div><p>x</p></div>",
                Limits {
                    depth: 1,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        for n in &d.nodes {
            assert!(n.subtree_end_exclusive > n.id && n.subtree_end_exclusive <= d.nodes.len());
            if let Some(p) = n.parent {
                assert!(n.id < d.nodes[p].subtree_end_exclusive);
            }
        }
        let again = observe(html, Limits::default()).unwrap();
        assert_eq!(
            serde_json::to_vec(&d).unwrap(),
            serde_json::to_vec(&again).unwrap()
        );
    }
}

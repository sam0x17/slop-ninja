//! Explicit, contiguous paragraph selections from an observed HTML source.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    html_document::{self, Limits, Value},
    util::digest,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paragraph {
    pub ordinal: usize,
    pub node_id: usize,
    pub text_node_ids: Vec<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passage {
    pub schema: String,
    pub document_view: String,
    pub parser: String,
    pub source_html_sha256: String,
    pub start_node: usize,
    pub end_node_exclusive: usize,
    pub paragraphs: Vec<Paragraph>,
    pub text: String,
    pub text_sha256: String,
    pub normalization: String,
}

/// Select whole top-level paragraphs using inclusive selector ordinals.
/// Refuse intervening regions or unwrapped prose rather than deleting them.
/// This normalized text view does not establish attribution, coherence or origin.
pub fn paragraph_run(html: &str, limits: Limits, first: usize, last: usize) -> Result<Passage> {
    ensure!(first <= last, "Reversed paragraph range");
    let document = html_document::observe(html, limits)?;
    ensure!(
        document.parse_errors.is_empty(),
        "Review HTML recovery before selecting passages"
    );
    let start = document
        .nodes
        .iter()
        .find(|n| n.paragraph_ordinal == Some(first))
        .context("Missing first paragraph")?;
    let end = document
        .nodes
        .iter()
        .find(|n| n.paragraph_ordinal == Some(last))
        .context("Missing last paragraph")?;
    ensure!(
        start.region_parent.is_none() && end.region_parent.is_none(),
        "Selection is inside another region"
    );
    ensure!(
        start.id <= end.id && start.parent == end.parent,
        "Paragraph boundaries do not share document order and parent"
    );
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    for node in &document.nodes[start.id..end.subtree_end_exclusive] {
        if !node.roles.is_empty() {
            ensure!(
                node.roles == ["paragraph"]
                    && node.region_parent.is_none()
                    && node.parent == start.parent,
                "Selection crosses a nonparagraph or nested region at node {}",
                node.id
            );
            paragraphs.push(Paragraph {
                ordinal: node.paragraph_ordinal.context("Paragraph has no ordinal")?,
                node_id: node.id,
                text_node_ids: Vec::new(),
                text: String::new(),
            });
        }
        match &node.value {
            Value::Text { decoded } => {
                let p = paragraphs
                    .last_mut()
                    .context("Text before first paragraph")?;
                if node.region_parent == Some(p.node_id) {
                    p.text.push_str(decoded);
                    p.text_node_ids.push(node.id);
                } else {
                    ensure!(
                        decoded.trim().is_empty(),
                        "Unwrapped text between paragraphs"
                    );
                }
            }
            Value::Element {
                name, namespace, ..
            } => {
                ensure!(
                    namespace == "http://www.w3.org/1999/xhtml",
                    "Foreign namespace inside passage"
                );
                match name.as_str() {
                    "p" | "a" | "abbr" | "b" | "bdi" | "bdo" | "cite" | "em" | "i" | "mark"
                    | "small" | "span" | "strong" | "sub" | "sup" | "u" | "s" | "del" | "ins"
                    | "time" | "wbr" => {}
                    "br" => {
                        let p = paragraphs.last_mut().context("Break before paragraph")?;
                        ensure!(
                            node.region_parent == Some(p.node_id),
                            "Break outside paragraph"
                        );
                        p.text.push(' ');
                    }
                    _ => bail!("Unsupported inline element {name} at node {}", node.id),
                }
            }
            Value::Comment { .. } => {}
            _ => bail!("Unsupported node inside paragraph range"),
        }
    }
    ensure!(
        paragraphs.iter().map(|p| p.ordinal).eq(first..=last),
        "Paragraph order or coverage changed"
    );
    for p in &mut paragraphs {
        p.text = p.text.split_whitespace().collect::<Vec<_>>().join(" ");
        ensure!(!p.text.is_empty(), "Empty paragraph in selection");
    }
    let text = paragraphs
        .iter()
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Passage {
        schema: "slop_ninja_html_paragraph_run_v1".to_owned(),
        document_view: document.schema,
        parser: document.parser,
        source_html_sha256: document.source_html_sha256,
        start_node: start.id,
        end_node_exclusive: end.subtree_end_exclusive,
        paragraphs,
        text_sha256: digest(&text),
        text,
        normalization: "Decoded DOM text; br becomes a space; Unicode whitespace collapses within each paragraph; paragraphs joined by two LF. Inline markup and comments remain in the source view, not the plain text. Node IDs are DOM positions, not source byte offsets. This is not browser rendering.".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_paragraphs_keep_quoted_terms_and_explicit_breaks() {
        let html = "<div class='wp-caption'><p>Photo credit.</p></div><p>Lee called it “HOME” &amp; meant <em>home</em>.<br>Yes.</p>\n<p>The\u{a0}next point stays.</p><blockquote><p>Separate speaker.</p></blockquote>";
        let p = paragraph_run(html, Limits::default(), 1, 2).unwrap();
        assert_eq!(
            p.text,
            "Lee called it “HOME” & meant home. Yes.\n\nThe next point stays."
        );
        assert_eq!(
            p.paragraphs.iter().map(|p| p.ordinal).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(p.source_html_sha256, digest(html));
    }

    #[test]
    fn quotes_media_and_other_intervening_regions_cannot_be_skipped() {
        let html = "<p>Introduction.</p><blockquote><p>Answer.</p></blockquote><p>Conclusion.</p>";
        assert!(paragraph_run(html, Limits::default(), 0, 2).is_err());
        assert!(paragraph_run(html, Limits::default(), 1, 1).is_err());
        for embedded in [
            "<q>Other words</q>",
            "<img src='local.png'>",
            "<code>x &lt; 3</code>",
            "<input value='label'>",
        ] {
            assert!(
                paragraph_run(
                    &format!("<p>Before {embedded} after.</p>"),
                    Limits::default(),
                    0,
                    0
                )
                .is_err()
            );
        }
    }

    #[test]
    fn missing_boundaries_unwrapped_text_and_recovery_remain_errors() {
        for (html, first, last) in [
            ("<p>First.</p>Outside<p>Second.</p>", 0, 1),
            ("<p>Only.</p>", 0, 1),
            ("<p>Only.</p>", 1, 0),
            ("<p> </p>", 0, 0),
            ("<p id='a' id='b'>Recovered.</p>", 0, 0),
        ] {
            assert!(paragraph_run(html, Limits::default(), first, last).is_err());
        }
    }
}

//! An explicit CommonMark text view with UTF-8 source provenance.
//!
//! Syntax annotations belong to the rendered text. Project their spans before
//! considering an edit to the original source. A verbatim projection establishes
//! byte correspondence only; it does not license a meaning-preserving rewrite.

use anyhow::{Context, Result, ensure};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ops::Range;

pub const VERSION: &str = "slop_ninja_commonmark_text_view_v1";
pub const POLICY: &str = "pulldown-cmark=0.13.4;options=none;soft-break=space;hard-break=newline;blocks=blank-line;code=protected-text;html-and-images=unsupported";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MappingKind {
    Verbatim,
    Normalized,
    Generated,
}

#[derive(Clone, Debug, Serialize)]
pub struct Mapping {
    pub view: Range<usize>,
    pub source: Option<Range<usize>>,
    pub kind: MappingKind,
    pub protected: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Region {
    pub kind: String,
    pub source: Range<usize>,
}

/// Construction is private: a returned view has validated, immutable mappings.
#[derive(Clone, Debug, Serialize)]
pub struct MarkdownView {
    schema: &'static str,
    policy: &'static str,
    source_sha256: String,
    source: String,
    text: String,
    mappings: Vec<Mapping>,
    regions: Vec<Region>,
    unsupported: Vec<Region>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Projection {
    pub view: Range<usize>,
    pub fragments: Vec<Mapping>,
    /// Only contiguous, unprotected verbatim bytes qualify. This is not edit approval.
    pub verbatim_source_range: Option<Range<usize>>,
}

impl MarkdownView {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn mappings(&self) -> &[Mapping] {
        &self.mappings
    }

    pub fn unsupported(&self) -> &[Region] {
        &self.unsupported
    }

    /// Map a nonempty rendered span without pretending normalization is reversible.
    pub fn project(&self, span: Range<usize>) -> Result<Projection> {
        ensure!(span.start < span.end, "Projection needs a nonempty span");
        let rendered = self
            .text
            .get(span.clone())
            .context("Invalid view UTF-8 span")?;
        let mut fragments = Vec::new();
        let first = self
            .mappings
            .partition_point(|mapping| mapping.view.end <= span.start);
        for mapping in self.mappings[first..]
            .iter()
            .take_while(|mapping| mapping.view.start < span.end)
        {
            let start = span.start.max(mapping.view.start);
            let end = span.end.min(mapping.view.end);
            if start >= end {
                continue;
            }
            let source = if mapping.kind == MappingKind::Verbatim {
                let original = mapping.source.as_ref().context("Missing verbatim source")?;
                Some(
                    original.start + start - mapping.view.start
                        ..original.start + end - mapping.view.start,
                )
            } else {
                // A partial decoded entity still names its whole original event.
                mapping.source.clone()
            };
            fragments.push(Mapping {
                view: start..end,
                source,
                kind: mapping.kind.clone(),
                protected: mapping.protected,
            });
        }
        let mut contiguous: Option<Range<usize>> = None;
        let mut exact = true;
        for fragment in &fragments {
            if fragment.kind != MappingKind::Verbatim || fragment.protected {
                exact = false;
                break;
            }
            let source = fragment
                .source
                .as_ref()
                .context("Missing fragment source")?;
            if let Some(range) = &mut contiguous {
                if range.end != source.start {
                    exact = false;
                    break;
                }
                range.end = source.end;
            } else {
                contiguous = Some(source.clone());
            }
        }
        let verbatim_source_range =
            contiguous.filter(|range| exact && self.source.get(range.clone()) == Some(rendered));
        Ok(Projection {
            view: span,
            fragments,
            verbatim_source_range,
        })
    }

    fn append(&mut self, text: &str, source: Option<Range<usize>>, protected: bool) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let kind = match &source {
            Some(span) => {
                let original = self
                    .source
                    .get(span.clone())
                    .context("Invalid parser source span")?;
                if original == text {
                    MappingKind::Verbatim
                } else {
                    MappingKind::Normalized
                }
            }
            None => MappingKind::Generated,
        };
        let start = self.text.len();
        self.text.push_str(text);
        self.mappings.push(Mapping {
            view: start..self.text.len(),
            source,
            kind,
            protected,
        });
        Ok(())
    }

    fn content(
        &mut self,
        text: &str,
        source: Range<usize>,
        protected: bool,
        pending: &mut bool,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if *pending && !self.text.is_empty() {
            // Do not collapse repeated literal newlines inside a code block.
            let separator = if self.text.ends_with("\n\n") {
                ""
            } else if self.text.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            self.append(separator, None, false)?;
        }
        *pending = false;
        self.append(text, Some(source), protected)
    }
}

/// Render the declared CommonMark profile. Unsupported constructs remain explicit.
pub fn render(source: &str) -> Result<MarkdownView> {
    let mut view = MarkdownView {
        schema: VERSION,
        policy: POLICY,
        source_sha256: hex::encode(Sha256::digest(source.as_bytes())),
        source: source.to_owned(),
        text: String::new(),
        mappings: Vec::new(),
        regions: Vec::new(),
        unsupported: Vec::new(),
    };
    let mut pending_break = false;
    let mut code_depth = 0usize;
    let mut image_depth = 0usize;
    for (event, span) in Parser::new(source).into_offset_iter() {
        ensure!(
            source.get(span.clone()).is_some(),
            "Parser returned an invalid UTF-8 span"
        );
        match event {
            Event::Start(tag) => {
                let kind = match tag {
                    Tag::Paragraph => "paragraph",
                    Tag::Heading { .. } => "heading",
                    Tag::BlockQuote(_) => "block_quote",
                    Tag::List(_) => "list",
                    Tag::Item => "item",
                    Tag::Emphasis => "emphasis",
                    Tag::Strong => "strong",
                    Tag::Link { .. } => "link",
                    Tag::CodeBlock(_) => {
                        code_depth += 1;
                        "code_block"
                    }
                    Tag::Image { .. } => {
                        image_depth += 1;
                        view.unsupported.push(Region {
                            kind: "image".into(),
                            source: span.clone(),
                        });
                        "image"
                    }
                    Tag::HtmlBlock => "html_block",
                    _ => {
                        view.unsupported.push(Region {
                            kind: "unhandled_container".into(),
                            source: span.clone(),
                        });
                        "unhandled_container"
                    }
                };
                view.regions.push(Region {
                    kind: kind.into(),
                    source: span,
                });
            }
            Event::End(tag) => match tag {
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::BlockQuote(_)
                | TagEnd::List(_)
                | TagEnd::HtmlBlock => pending_break = true,
                TagEnd::CodeBlock => {
                    code_depth = code_depth.checked_sub(1).context("Unbalanced code block")?;
                    pending_break = true;
                }
                TagEnd::Image => {
                    image_depth = image_depth.checked_sub(1).context("Unbalanced image")?
                }
                _ => {}
            },
            Event::Text(text) if image_depth == 0 => {
                view.content(&text, span, code_depth > 0, &mut pending_break)?
            }
            Event::Code(text) if image_depth == 0 => {
                view.regions.push(Region {
                    kind: "inline_code".into(),
                    source: span.clone(),
                });
                view.content(&text, span, true, &mut pending_break)?;
            }
            Event::SoftBreak if image_depth == 0 => {
                view.content(" ", span, false, &mut pending_break)?
            }
            Event::HardBreak if image_depth == 0 => {
                view.content("\n", span, false, &mut pending_break)?
            }
            Event::Rule => {
                view.regions.push(Region {
                    kind: "thematic_break".into(),
                    source: span,
                });
                pending_break = true;
            }
            Event::Html(_) | Event::InlineHtml(_) => {
                view.unsupported.push(Region {
                    kind: "raw_html".into(),
                    source: span,
                });
                pending_break = true;
            }
            Event::Text(_) | Event::Code(_) | Event::SoftBreak | Event::HardBreak => {}
            _ => view.unsupported.push(Region {
                kind: "unhandled_event".into(),
                source: span,
            }),
        }
    }
    ensure!(
        code_depth == 0 && image_depth == 0,
        "Unbalanced Markdown containers"
    );
    let mut cursor = 0;
    for mapping in &view.mappings {
        ensure!(
            mapping.view.start == cursor && mapping.view.end > cursor,
            "Incomplete view mapping"
        );
        cursor = mapping.view.end;
    }
    ensure!(cursor == view.text.len(), "Unmapped rendered bytes");
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(view: &MarkdownView, text: &str) -> Range<usize> {
        let start = view.text().find(text).unwrap();
        start..start + text.len()
    }

    #[test]
    fn emphasis_exposes_the_predicate_without_changing_its_source() -> Result<()> {
        let source = "# Test notes\n\nThe barrier—called *the screen*—collapsed.\n";
        let view = render(source)?;
        assert_eq!(view.source(), source);
        assert_eq!(
            view.text(),
            "Test notes\n\nThe barrier—called the screen—collapsed."
        );
        let projected = view.project(word(&view, "collapsed"))?;
        let range = projected.verbatim_source_range.unwrap();
        assert_eq!(&source[range], "collapsed");
        assert!(view.unsupported().is_empty());
        Ok(())
    }

    #[test]
    fn normalized_protected_and_disjoint_spans_cannot_pose_as_verbatim_edits() -> Result<()> {
        let view = render("R&amp;D is un**likely** to alter `x*y`. A \\* stays literal.")?;
        assert_eq!(
            view.text(),
            "R&D is unlikely to alter x*y. A * stays literal."
        );
        for text in ["R&D", "unlikely", "x*y"] {
            assert!(
                view.project(word(&view, text))?
                    .verbatim_source_range
                    .is_none(),
                "{text}"
            );
        }
        assert!(
            view.project(word(&view, "likely"))?
                .verbatim_source_range
                .is_some()
        );
        let literal = render("unclosed *marker and snake_case remain literal")?;
        assert_eq!(
            literal.text(),
            "unclosed *marker and snake_case remain literal"
        );
        Ok(())
    }

    #[test]
    fn block_layout_and_link_labels_have_explicit_provenance() -> Result<()> {
        let source = "> First line\n> continues.\n\n- Read [the guide](https://example.org).\n- Then test.\n\n```rust\nx*y\n```\n";
        let view = render(source)?;
        assert_eq!(
            view.text(),
            "First line continues.\n\nRead the guide.\n\nThen test.\n\nx*y\n"
        );
        let label = view
            .project(word(&view, "the guide"))?
            .verbatim_source_range
            .unwrap();
        assert_eq!(&source[label], "the guide");
        assert!(
            view.project(word(&view, "x*y"))?
                .verbatim_source_range
                .is_none()
        );
        assert!(
            view.mappings()
                .iter()
                .any(|mapping| mapping.kind == MappingKind::Generated)
        );
        Ok(())
    }

    #[test]
    fn unsupported_content_and_invalid_unicode_spans_remain_visible() -> Result<()> {
        let view = render("café ![diagram](plot.png) <span>text</span>")?;
        assert!(
            view.unsupported()
                .iter()
                .any(|region| region.kind == "image")
        );
        assert!(
            view.unsupported()
                .iter()
                .any(|region| region.kind == "raw_html")
        );
        assert!(view.project(0..4).is_err()); // Ends inside é.
        assert!(view.project(0..0).is_err());
        assert!(view.project(0..usize::MAX).is_err());
        Ok(())
    }
}

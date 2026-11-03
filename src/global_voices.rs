//! Publisher metadata and conservative paragraph extraction for a local corpus.
//! A single byline is attribution evidence, not proof of unaided authorship.

use anyhow::{Context, Result, ensure};
use scraper::{ElementRef, Html, Node, Selector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const EXTRACTION: &str = "global-voices-paragraph-filter-v2";

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("static selector")
}

fn text(element: ElementRef<'_>) -> String {
    let mut raw = String::new();
    for node in element.descendants() {
        match node.value() {
            Node::Text(value) => raw.push_str(value),
            Node::Element(value) if ["br", "p", "div", "blockquote"].contains(&value.name()) => {
                raw.push(' ')
            }
            _ => {}
        }
    }
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn source_url(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("globalvoices.org")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none(),
        "Expected a Global Voices HTTPS URL"
    );
    ensure!(
        !["/wp-admin/", "/wp-includes/", "/site/"]
            .iter()
            .any(|prefix| url.path().starts_with(prefix)),
        "Path excluded by publisher robots policy"
    );
    Ok(url)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    pub url: String,
    pub name: String,
    pub displayed_posts: usize,
}

pub fn directory(html: &str) -> Result<Vec<Author>> {
    let document = Html::parse_document(html);
    let mut authors = Vec::new();
    let mut seen = BTreeSet::new();
    for element in document.select(&selector(".user-list-summary .author-block")) {
        let name = element
            .select(&selector(".username"))
            .next()
            .context("Missing directory name")?;
        let url = element
            .select(&selector("a.user-link"))
            .next()
            .and_then(|a| a.value().attr("href"))
            .context("Missing author URL")?;
        source_url(url)?;
        ensure!(seen.insert(url.to_owned()), "Duplicate directory author");
        let count = element
            .select(&selector(".user-post-count strong"))
            .next()
            .context("Missing post count")?;
        authors.push(Author {
            url: url.to_owned(),
            name: text(name),
            displayed_posts: text(count).replace(',', "").parse()?,
        });
    }
    ensure!(!authors.is_empty(), "No directory authors matched");
    Ok(authors)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credit {
    pub label: String,
    pub writers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchivePost {
    pub url: String,
    pub title: String,
    pub credits: Vec<Credit>,
    pub displayed_date: String,
}

impl ArchivePost {
    pub fn sole_english_writer(&self, expected: &str) -> bool {
        self.credits.len() == 1
            && self.credits[0].label == "Written by"
            && self.credits[0].writers == [expected]
    }
}

pub fn archive(html: &str) -> Result<(u64, Vec<ArchivePost>)> {
    let document = Html::parse_document(html);
    let body = document
        .select(&selector("body"))
        .next()
        .context("Missing body")?;
    let author_id = body
        .value()
        .classes()
        .filter_map(|class| class.strip_prefix("author-")?.parse::<u64>().ok())
        .next()
        .context("Missing numeric publisher author ID")?;
    let mut posts = Vec::new();
    for card in document.select(&selector(".post-archive .gv-post-promo-card")) {
        let link = card
            .select(&selector(".post-title a[rel='bookmark']"))
            .next()
            .context("Missing story link")?;
        let url = link.value().attr("href").context("Missing story href")?;
        source_url(url)?;
        let mut credits = Vec::new();
        for credit in card.select(&selector(".post-promo-credits .text-credits-section")) {
            let label = credit
                .select(&selector(".credit-label"))
                .next()
                .context("Missing credit role")?;
            let writers = credit
                .select(&selector("a.user-link"))
                .map(|a| {
                    a.value()
                        .attr("href")
                        .context("Missing credit URL")
                        .map(str::to_owned)
                })
                .collect::<Result<Vec<_>>>()?;
            credits.push(Credit {
                label: text(label),
                writers,
            });
        }
        let date = card
            .select(&selector(".datestamp"))
            .next()
            .context("Missing displayed story date")?;
        posts.push(ArchivePost {
            url: url.to_owned(),
            title: text(link),
            credits,
            displayed_date: text(date),
        });
    }
    ensure!(!posts.is_empty(), "No archive stories matched");
    Ok((author_id, posts))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Paragraph {
    pub ordinal: usize,
    pub text: String,
    pub excluded: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Extraction {
    pub version: String,
    pub paragraphs: Vec<Paragraph>,
    pub text: String,
    pub origin_flags: Vec<String>,
}

pub fn extract_content(html: &str) -> Extraction {
    let document = Html::parse_fragment(html);
    let notices = [
        "republish",
        "originally published",
        "first published",
        "first appeared",
        "content partnership",
        "reproduced with",
        "used with permission",
        "translated from",
    ];
    let mut paragraphs = Vec::new();
    for (ordinal, p) in document.select(&selector("p")).enumerate() {
        let value = text(p);
        let embedded = p.ancestors().filter_map(ElementRef::wrap).any(|a| {
            [
                "blockquote",
                "figure",
                "figcaption",
                "table",
                "li",
                "script",
                "style",
            ]
            .contains(&a.value().name())
                || a.value().classes().any(|class| {
                    [
                        "wp-caption",
                        "wp-caption-text",
                        "factbox",
                        "translation",
                        "inline-rss",
                    ]
                    .contains(&class)
                })
        }) || p.value().classes().any(|class| class == "wp-caption-text")
            || p.select(&selector("script, style, iframe, q, img"))
                .next()
                .is_some();
        let foreign = p
            .ancestors()
            .filter_map(ElementRef::wrap)
            .chain(std::iter::once(p))
            .any(|a| {
                a.value()
                    .attr("lang")
                    .is_some_and(|lang| lang != "en" && !lang.starts_with("en-"))
            });
        let excluded = if embedded {
            Some("embedded_or_nonprose")
        } else if foreign {
            Some("explicit_other_language")
        } else if value.contains(['"', '“', '”', '‘', '«', '»']) || single_quoted(&value) {
            Some("quotation_marked_paragraph")
        } else if value.is_empty() {
            Some("empty")
        } else {
            None
        };
        paragraphs.push(Paragraph {
            ordinal,
            text: value,
            excluded: excluded.map(str::to_owned),
        });
    }
    let body_text = paragraphs
        .iter()
        .filter(|p| p.excluded.as_deref() != Some("embedded_or_nonprose"))
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let origin_flags = notices
        .iter()
        .filter(|term| body_text.contains(**term))
        .map(|s| (*s).to_owned())
        .collect();
    let text = paragraphs
        .iter()
        .filter(|p| p.excluded.is_none())
        .map(|p| p.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    Extraction {
        version: EXTRACTION.to_owned(),
        paragraphs,
        text,
        origin_flags,
    }
}

fn single_quoted(value: &str) -> bool {
    static PATTERN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?:^|\s)'[^']+'(?:\s|[.,;!?]|$)").unwrap());
    PATTERN.is_match(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_filter_keeps_exact_inline_punctuation_and_separate_blocks() {
        let x = extract_content(
            "<p>A <b>plain</b> sentence &amp; punctuation.</p><blockquote><p>Another person's text.</p></blockquote><div class='wp-caption'><p>Caption</p></div><p>She said “yes”.</p><p>One’s own apostrophe remains.</p><p lang='es'>Otra frase.</p>",
        );
        assert_eq!(
            x.text,
            "A plain sentence & punctuation.\n\nOne’s own apostrophe remains."
        );
        assert_eq!(
            x.paragraphs.iter().filter(|p| p.excluded.is_some()).count(),
            4
        );
        assert_eq!(
            extract_content("<p>First<br>second; micro<b>scope</b>.</p>").text,
            "First second; microscope."
        );
        assert!(extract_content("<div class='wp-caption'><p>Photo used with permission.</p></div><p>Original prose.</p>").origin_flags.is_empty());
        assert!(
            extract_content("<p>She said 'hello'.</p><p>He said «hello».</p>")
                .text
                .is_empty()
        );
        assert!(
            !extract_content("<p><em>This was republished under a content partnership.</em></p>")
                .origin_flags
                .is_empty()
        );
    }

    #[test]
    fn archive_metadata_distinguishes_translation_and_multiple_writers() {
        let make = |credit: &str| {
            format!(
                "<body class='author-synthetic author-42'><div class='post-archive'><article class='gv-post-promo-card'><h3 class='post-title'><a rel='bookmark' href='https://globalvoices.org/2020/01/01/example/'>Example</a></h3><div class='post-promo-credits'>{credit}</div><span class='datestamp'>1 January 2020</span></article></div></body>"
            )
        };
        let own = "<div class='text-credits-section'><span class='credit-label'>Written by</span><a class='user-link' href='https://globalvoices.org/author/example/'>Example</a></div>";
        let (id, posts) = archive(&make(own)).unwrap();
        assert_eq!(id, 42);
        assert!(posts[0].sole_english_writer("https://globalvoices.org/author/example/"));
        let translated = own.replace("Written by", "Translated (English) by");
        assert!(
            !archive(&make(&translated)).unwrap().1[0]
                .sole_english_writer("https://globalvoices.org/author/example/")
        );
        assert!(
            !archive(&make(&format!("{own}{own}"))).unwrap().1[0]
                .sole_english_writer("https://globalvoices.org/author/example/")
        );
        assert!(source_url("https://globalvoices.org.evil.test/example").is_err());
        assert!(source_url("https://globalvoices.org/wp-admin/anything").is_err());
    }
}

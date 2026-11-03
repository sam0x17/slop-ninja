//! Conservative historical Wikipedia text matching.
use anyhow::{Context, Result, ensure};
use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::Value;
use std::{fs, ops::RangeInclusive, path::Path};

/// Recheck an archived capture without trusting paths or hashes in a review.
pub fn captured_bytes(root: &Path, meta: &Value) -> Result<Vec<u8>> {
    let relative = Path::new(meta["raw_path"].as_str().context("Missing capture path")?);
    ensure!(
        relative
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_))),
        "Unsafe capture path"
    );
    let bytes = fs::read(root.join(relative))?;
    ensure!(
        meta["status"] == 200 && meta["body_sha256"] == crate::dataset::sha256(&bytes),
        "Capture status/hash mismatch"
    );
    Ok(bytes)
}

pub fn captured_json(root: &Path, meta: &Value) -> Result<Value> {
    Ok(serde_json::from_slice(&captured_bytes(root, meta)?)?)
}

/// Conservative admission screen shared by historical lead and plot excerpts.
pub fn screen_revision(
    review: &Value,
    raw: &Value,
    rendered: &Value,
    text: &str,
    cutoff: &str,
) -> Result<()> {
    screen_revision_in_word_range(review, raw, rendered, text, cutoff, 80..=500)
}

/// Change only the declared length range; retain every historical and rights screen.
pub fn screen_revision_in_word_range(
    review: &Value,
    raw: &Value,
    rendered: &Value,
    text: &str,
    cutoff: &str,
    words: RangeInclusive<usize>,
) -> Result<()> {
    ensure!(
        !words.is_empty() && *words.start() > 0,
        "Invalid lexical-word range"
    );
    let page = &raw["query"]["pages"][0];
    let revision = &page["revisions"][0];
    ensure!(
        page["pageid"] == review["page_id"]
            && revision["revid"] == review["revision_id"]
            && rendered["parse"]["revid"] == review["revision_id"],
        "Source revision mismatch"
    );
    let timestamp = revision["timestamp"]
        .as_str()
        .context("Missing revision timestamp")?;
    ensure!(timestamp <= cutoff, "Source after historical cutoff");
    let wiki = revision["slots"]["main"]["content"]
        .as_str()
        .context("Missing revision text")?;
    let html = rendered["parse"]["text"]
        .as_str()
        .context("Missing rendered text")?;
    let normalized = normalize(text);
    ensure!(
        !normalized.is_empty()
            && literal_text(wiki).contains(&normalized)
            && lead_text(html).contains(&normalized),
        "Full historical text mismatch"
    );
    ensure!(
        words.contains(&grammar_core::features::words(text).len()),
        "Outside {}..={} lexical-word range",
        words.start(),
        words.end()
    );
    let lowered = wiki.to_lowercase();
    for term in [
        "copyvio",
        "copyright",
        "fair use",
        "fairuse",
        "cc-notice",
        "cc-by",
        "attribution",
        "copied",
        "public domain",
        "pd-notice",
        "eb1911",
        "gfdl",
        "plagiar",
        "permission=",
    ] {
        ensure!(
            !lowered.contains(term),
            "Source notice requires separate review: {term}"
        );
    }
    let doc = Html::parse_fragment(html);
    let boxes = Selector::parse(".ambox .mbox-text").unwrap();
    ensure!(
        !doc.select(&boxes).any(|element| {
            let warning = element.text().collect::<String>().to_lowercase();
            [
                "copyright",
                "copied",
                "attribution",
                "license",
                "permission",
            ]
            .iter()
            .any(|t| warning.contains(t))
        }),
        "Rendered copyright warning"
    );
    let quotes = Regex::new(r#"["“«]([^"”»\n]+)["”»]"#)?;
    ensure!(
        !quotes
            .captures_iter(text)
            .any(|c| c[1].split_whitespace().count() >= 8),
        "Long quotation requires separate review"
    );
    Ok(())
}

pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Deliberately partial. Unhandled markup creates a mismatch, never a guessed
// completion. No templates are expanded, so rendered-at-retrieval additions
// cannot by themselves establish historical wording.
pub fn literal_text(wikitext: &str) -> String {
    let mut text = wikitext.to_owned();
    for pattern in [
        r"(?s)<!--.*?-->",
        r"(?is)<ref\b[^>]*?/>|<ref\b[^>]*>.*?</ref\s*>",
    ] {
        text = Regex::new(pattern)
            .unwrap()
            .replace_all(&text, "")
            .into_owned();
    }
    text = Regex::new(r"\[\[(?:[^\[\]|]+\|)?([^\[\]]+)\]\]")
        .unwrap()
        .replace_all(&text, "$1")
        .into_owned();
    text = text.replace("'''", "").replace("''", "");
    // Decode character entities and omit remaining HTML tags without rendering
    // MediaWiki templates, parser functions or their parameter values.
    normalize(
        &Html::parse_fragment(&text)
            .root_element()
            .text()
            .collect::<String>(),
    )
}

pub fn lead_text(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let selector = Selector::parse(".mw-parser-output > p").unwrap();
    let paragraphs: Vec<_> = doc
        .select(&selector)
        .map(|p| {
            p.descendants()
                .filter(|node| {
                    !node.ancestors().filter_map(ElementRef::wrap).any(|a| {
                        ["sup", "style", "script", "table"].contains(&a.value().name())
                            || a.value()
                                .classes()
                                .any(|c| ["reference", "noprint", "mw-editsection"].contains(&c))
                    })
                })
                .filter_map(|n| {
                    if let Node::Text(t) = n.value() {
                        Some::<&str>(t)
                    } else {
                        None
                    }
                })
                .collect::<String>()
        })
        .collect();
    normalize(&paragraphs.join(" "))
}

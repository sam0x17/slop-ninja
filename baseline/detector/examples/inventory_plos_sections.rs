//! Inspect cached JATS sections without retrieving, admitting, or scoring text.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use reqwest::Url;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, sha256};
use std::{collections::BTreeSet, fs, io::Write, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long, required = true)]
    acquisition: Vec<PathBuf>,
    #[arg(long)]
    historical_ledger: PathBuf,
    #[arg(long)]
    expected_ledger_sha256: String,
    #[arg(long)]
    exclude_corpus: Vec<PathBuf>,
    #[arg(long)]
    output: PathBuf,
}

fn text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|n| n.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn child<'a, 'b>(node: Node<'a, 'b>, name: &str) -> Result<Node<'a, 'b>> {
    node.children()
        .find(|n| n.has_tag_name(name))
        .with_context(|| format!("Missing {name}"))
}

fn explicit_cc_by(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    matches!(url.scheme(), "https" | "http")
        && url.host_str() == Some("creativecommons.org")
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(
            url.path().trim_end_matches('/'),
            "/licenses/by/2.5" | "/licenses/by/3.0" | "/licenses/by/4.0"
        )
}

fn inspect(xml: &str, requested_doi: &str, excluded: &BTreeSet<String>) -> Result<Value> {
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )?;
    let article = doc.root_element();
    ensure!(article.has_tag_name("article"), "Not article XML");
    let meta = child(child(article, "front")?, "article-meta")?;
    let doi = meta
        .children()
        .find(|n| n.has_tag_name("article-id") && n.attribute("pub-id-type") == Some("doi"))
        .map(text)
        .context("Missing DOI")?;
    ensure!(doi == requested_doi, "DOI differs from cached request");
    let source_group = format!("doi:{}", doi.to_lowercase());
    let title = text(child(child(meta, "title-group")?, "article-title")?);
    let year = meta
        .children()
        .find(|n| n.has_tag_name("pub-date") && n.attribute("pub-type") == Some("epub"))
        .and_then(|n| child(n, "year").ok())
        .map(text);
    let license_urls: BTreeSet<String> = meta
        .children()
        .filter(|n| n.has_tag_name("permissions"))
        .flat_map(|n| n.children().filter(|c| c.has_tag_name("license")))
        .flat_map(|n| n.descendants())
        .filter_map(|n| n.attribute(("http://www.w3.org/1999/xlink", "href")))
        .map(str::to_owned)
        .collect();
    let mut sections = Vec::new();
    for (index, section) in child(article, "body")?
        .children()
        .filter(|n| n.has_tag_name("sec"))
        .enumerate()
    {
        let heading = child(section, "title").map(text).unwrap_or_default();
        let named_intro = matches!(
            heading.to_lowercase().as_str(),
            "introduction" | "background"
        );
        if section.attribute("sec-type") != Some("intro") && !named_intro {
            continue;
        }
        let mut paragraphs = Vec::new();
        for p in section.descendants().filter(|n| n.has_tag_name("p")) {
            let excluded_container = p.ancestors().take_while(|n| *n != section).any(|n| {
                ["fig", "table-wrap", "fn", "supplementary-material"]
                    .iter()
                    .any(|tag| n.has_tag_name(*tag))
            });
            if !excluded_container {
                paragraphs.push(text(p));
            }
        }
        let joined = paragraphs.join("\n\n");
        let words = joined.split_whitespace().count();
        let mut markup = serde_json::Map::new();
        for tag in [
            "xref",
            "disp-formula",
            "inline-formula",
            "math",
            "tex-math",
            "disp-quote",
            "quoted-text",
            "permissions",
            "license",
            "fig",
            "table-wrap",
            "fn",
            "list",
            "supplementary-material",
        ] {
            let count = section
                .descendants()
                .filter(|n| n.has_tag_name(tag))
                .count();
            if count > 0 {
                markup.insert(tag.into(), json!(count));
            }
        }
        sections.push(json!({
            "body_section_index":index,"heading":heading,"section_type":section.attribute("sec-type"),
            "raw_utf8_start":section.range().start,"raw_utf8_end":section.range().end,
            "raw_section_sha256":sha256(&xml.as_bytes()[section.range()]),
            "paragraphs":paragraphs.len(),"paragraph_whitespace_words":words,
            "paragraph_projection_sha256":sha256(joined.as_bytes()),
            "within_review_word_range":(650..=1600).contains(&words),
            "contains_double_quotation_marks":joined.chars().any(|c| matches!(c, '"' | '“' | '”' | '«' | '»' | '„' | '‟')),
            "markup":markup
        }));
    }
    Ok(json!({
        "source_group":source_group,"doi":doi,"title":title,"publication_year":year,
        "research_article":article.attribute("article-type")==Some("research-article"),
        "english":article.attribute(("http://www.w3.org/XML/1998/namespace","lang"))==Some("en"),
        "has_explicit_cc_by_link":license_urls.iter().any(|u| explicit_cc_by(u)),
        "license_links":license_urls,"already_in_exclusion_inputs":excluded.contains(&source_group),
        "sections":sections
    }))
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.output.exists(), "Use a new inventory output");
    let ledger_bytes = fs::read(&a.historical_ledger)?;
    ensure!(
        sha256(&ledger_bytes) == a.expected_ledger_sha256,
        "Changed historical ledger"
    );
    let ledger: Value = serde_json::from_slice(&ledger_bytes)?;
    let mut excluded: BTreeSet<_> = ledger["indexes"]["canonical_work"]
        .as_object()
        .context("Missing historical work index")?
        .keys()
        .map(|s| s.to_lowercase())
        .collect();
    ensure!(!excluded.is_empty(), "Empty historical work index");
    let mut bindings = vec![json!({"path":a.historical_ledger,"sha256":sha256(&ledger_bytes)})];
    for path in &a.exclude_corpus {
        for r in dataset::read_records(path)? {
            excluded.insert(r.source_group.to_lowercase());
        }
        bindings.push(json!({"path":path,"sha256":sha256(fs::read(path)?) }));
    }
    let mut rows = Vec::new();
    let mut seen_captures = BTreeSet::new();
    for acquisition in &a.acquisition {
        let mut files = fs::read_dir(acquisition.join("raw"))?
            .map(|entry| Ok(entry?.path()))
            .collect::<Result<Vec<_>>>()?;
        files.sort();
        for path in files
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let metadata_bytes = fs::read(&path)?;
            let m: Value = serde_json::from_slice(&metadata_bytes)?;
            let url = Url::parse(
                m["request_url"]
                    .as_str()
                    .context("Missing cached request URL")?,
            )?;
            if url.host_str() != Some("journals.plos.org") || url.path() != "/plosone/article/file"
            {
                continue;
            }
            let body_path = path.with_extension("body");
            let bytes = fs::read(&body_path)?;
            let body_hash = sha256(&bytes);
            ensure!(
                m["body_sha256"] == body_hash,
                "Changed cache body: {}",
                body_path.display()
            );
            ensure!(
                seen_captures.insert(path.canonicalize()?),
                "Repeated capture path"
            );
            let doi = url
                .query_pairs()
                .find(|(k, _)| k == "id")
                .map(|(_, v)| v.into_owned())
                .context("Missing request DOI")?;
            let result = if m["status"]
                .as_u64()
                .is_some_and(|s| (200..300).contains(&s))
            {
                std::str::from_utf8(&bytes)
                    .context("Non-UTF8 JATS")
                    .and_then(|xml| inspect(xml, &doi, &excluded))
            } else {
                Err(anyhow::anyhow!(
                    "Cached unsuccessful response: {}",
                    path.display()
                ))
            };
            rows.push(json!({
                "metadata_path":path,"metadata_sha256":sha256(&metadata_bytes),
                "raw_path":body_path,"raw_sha256":body_hash,"request_url":url.as_str(),
                "article":result.as_ref().ok(),"inspection_error":result.err().map(|e|e.to_string())
            }));
        }
    }
    ensure!(!rows.is_empty(), "No cached PLOS article captures");
    let articles: Vec<_> = rows
        .iter()
        .filter_map(|r| r["article"].as_object())
        .collect();
    let unseen: Vec<_> = articles
        .iter()
        .filter(|r| r["already_in_exclusion_inputs"] == false)
        .collect();
    let range_count = |items: &[&serde_json::Map<String, Value>]| -> usize {
        items
            .iter()
            .map(|r| {
                r["sections"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|s| s["within_review_word_range"] == true)
                    .count()
            })
            .sum()
    };
    let unseen_refs: Vec<_> = unseen.into_iter().copied().collect();
    let report = json!({
        "schema":"slop_ninja.plos_section_inventory.v1","source_files":bindings,
        "captured_articles":rows.len(),"inspected_articles":articles.len(),
        "inspection_errors":rows.len()-articles.len(),"unseen_article_captures":unseen_refs.len(),
        "sections_in_review_word_range":range_count(&articles),
        "unseen_sections_in_review_word_range":range_count(&unseen_refs),
        "review_whitespace_word_range":[650,1600],"rows":rows,
        "admitted_records":0,"network_calls":0,"model_calls":0,
        "limitations":[
            "Inventory only: a CC BY link does not complete item-level rights or provenance review.",
            "Publication year does not prove current JATS bytes are an unchanged historical version.",
            "Counts cover paragraph projections, exclude figure/table/footnote/supplement containers, and retain citation text. They are not detector-token counts or approved text extractions.",
            "Quotation and markup indicators are incomplete screens, not semantic or rights judgments.",
            "Multiple captures or sections from one DOI are one source family; no text or split is admitted here.",
            "Unused means absent from the supplied exclusion inputs, not unseen by foundation-model pretraining."
        ]
    });
    let mut out = fs::File::create_new(&a.output)?;
    serde_json::to_writer_pretty(&mut out, &report)?;
    out.write_all(b"\n")?;
    println!(
        "{}",
        json!({"captured_articles":report["captured_articles"],"inspected_articles":report["inspected_articles"],"unseen_article_captures":report["unseen_article_captures"],"sections_in_review_word_range":report["sections_in_review_word_range"],"unseen_sections_in_review_word_range":report["unseen_sections_in_review_word_range"],"admitted_records":0})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_keeps_sections_separate_and_exposes_omitted_containers() {
        let xml = r#"<article article-type="research-article" xml:lang="en"><front><article-meta><article-id pub-id-type="doi">10.1371/example</article-id><title-group><article-title>Example</article-title></title-group></article-meta></front><body><sec sec-type="intro"><title>Introduction</title><p>First claim <xref ref-type="bibr">[1]</xref>.</p><fig><caption><p>Caption must stay outside the paragraph projection.</p></caption></fig><sec><title>Context</title><p>Second claim.</p></sec></sec><sec><title>Background</title><p>A distinct section.</p><disp-quote><p>Quoted material.</p></disp-quote></sec></body></article>"#;
        let excluded = BTreeSet::from(["doi:10.1371/example".into()]);
        let result = inspect(xml, "10.1371/example", &excluded).unwrap();
        assert_eq!(result["already_in_exclusion_inputs"], true);
        assert_eq!(result["has_explicit_cc_by_link"], false);
        let sections = result["sections"].as_array().unwrap();
        assert_eq!(sections.len(), 2);
        assert_eq!(
            sections[0]["paragraph_projection_sha256"],
            sha256("First claim [1].\n\nSecond claim.")
        );
        assert_eq!(sections[0]["markup"]["fig"], 1);
        assert_eq!(sections[0]["markup"]["xref"], 1);
        assert_eq!(sections[1]["markup"]["disp-quote"], 1);
        assert!(inspect(xml, "10.1371/wrong", &excluded).is_err());
    }
}

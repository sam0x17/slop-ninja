//! Rebuild the human pilot from pinned, licensed publisher XML.
//!
//! This is a source-conditioned abstract pilot. Publication before 2020 is
//! useful provenance, not a guarantee about authorship or training exposure.

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::Client;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Component, Path},
    time::Duration,
};

const EXTRACTOR: &str = "plos-jats-abstract-paragraphs-v1";
const XLINK: &str = "http://www.w3.org/1999/xlink";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing nonempty {key}"))
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Result<Node<'a, 'input>> {
    node.children()
        .find(|n| n.has_tag_name(name))
        .with_context(|| format!("missing XML element {name}"))
}

fn plain(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|n| n.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_cc_by(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str() == Some("creativecommons.org")
            && matches!(url.path(), "/licenses/by/4.0/" | "/licenses/by/3.0/")
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

#[derive(Debug)]
struct Abstract {
    text: String,
    paragraphs: usize,
    doi: String,
    title: String,
    authors: Vec<String>,
    publication_date: String,
    license_urls: Vec<String>,
    license_statement: String,
}

fn extract_xml(bytes: &[u8]) -> Result<Abstract> {
    let xml = std::str::from_utf8(bytes).context("publisher XML is not UTF-8")?;
    // JATS contains a DOCTYPE. roxmltree does not fetch external DTDs.
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )
    .context("parse publisher JATS XML")?;
    let root = doc.root_element();
    ensure!(root.has_tag_name("article"), "expected JATS article");
    ensure!(
        root.attribute("article-type") == Some("research-article"),
        "not a research article"
    );
    ensure!(
        root.attribute((XML, "lang")) == Some("en"),
        "not an English article"
    );
    let meta = child(child(root, "front")?, "article-meta")?;
    let abstract_node = child(meta, "abstract")?;
    ensure!(
        !abstract_node.descendants().any(|node| {
            ["inline-formula", "disp-formula", "sup"]
                .iter()
                .any(|name| node.has_tag_name(*name))
        }),
        "abstract contains unsupported mathematical or superscript markup"
    );
    let mut paragraphs = Vec::new();
    for node in abstract_node.descendants().filter(|n| n.has_tag_name("p")) {
        ensure!(
            !node
                .ancestors()
                .skip(1)
                .take_while(|n| *n != abstract_node)
                .any(|n| n.has_tag_name("p")),
            "nested abstract paragraphs would duplicate text"
        );
        let text = plain(node);
        if !text.is_empty() {
            paragraphs.push(text);
        }
    }
    ensure!(!paragraphs.is_empty(), "empty abstract");
    let doi = meta
        .children()
        .find(|node| {
            node.has_tag_name("article-id") && node.attribute("pub-id-type") == Some("doi")
        })
        .map(plain)
        .context("article DOI is missing")?;
    let title = plain(child(child(meta, "title-group")?, "article-title")?);
    let mut authors = Vec::new();
    for group in meta.children().filter(|n| n.has_tag_name("contrib-group")) {
        for contrib in group
            .children()
            .filter(|n| n.has_tag_name("contrib") && n.attribute("contrib-type") == Some("author"))
        {
            let author = if let Ok(name) = child(contrib, "name") {
                ["given-names", "surname"]
                    .iter()
                    .filter_map(|tag| child(name, tag).ok())
                    .map(plain)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            } else if let Ok(collab) = child(contrib, "collab") {
                plain(collab)
            } else {
                bail!("unrecognized author representation");
            };
            ensure!(!author.is_empty(), "empty author name");
            authors.push(author);
        }
    }
    ensure!(!authors.is_empty(), "no credited authors");
    let date = meta
        .children()
        .find(|node| node.has_tag_name("pub-date") && node.attribute("pub-type") == Some("epub"))
        .context("missing electronic publication date")?;
    let year: i32 = plain(child(date, "year")?).parse()?;
    let month: u32 = plain(child(date, "month")?).parse()?;
    let day: u32 = plain(child(date, "day")?).parse()?;
    let publication_date = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .context("invalid publication date")?
        .to_string();
    ensure!(year < 2020, "human pilot requires publication before 2020");
    let license = child(child(meta, "permissions")?, "license")?;
    let license_urls: Vec<_> = license
        .descendants()
        .filter_map(|n| n.attribute((XLINK, "href")))
        .map(str::to_string)
        .collect();
    ensure!(
        license_urls.iter().any(|s| is_cc_by(s)),
        "no verified CC BY license URL"
    );
    Ok(Abstract {
        text: paragraphs.join("\n\n"),
        paragraphs: paragraphs.len(),
        doi,
        title,
        authors,
        publication_date,
        license_urls,
        license_statement: plain(license),
    })
}

fn verify_source(source: &Value, bytes: &[u8]) -> Result<Abstract> {
    ensure!(
        hash(bytes) == string(source, "source_sha256")?,
        "publisher XML hash differs from pinned source"
    );
    let extracted = extract_xml(bytes)?;
    ensure!(
        extracted.doi == string(source, "doi")?,
        "article DOI differs from manifest"
    );
    ensure!(
        extracted.title == string(source, "title")?,
        "article title differs from manifest"
    );
    ensure!(
        extracted.publication_date == string(source, "publication_date")?,
        "publication date differs from manifest"
    );
    ensure!(
        json!(extracted.authors) == source["authors"],
        "credited authors differ from manifest"
    );
    ensure!(
        extracted
            .license_urls
            .iter()
            .any(|url| url == source["license_url"].as_str().unwrap_or("")),
        "manifest license URL is absent from publisher XML"
    );
    ensure!(
        is_cc_by(string(source, "license_url")?),
        "manifest license is not CC BY"
    );
    ensure!(
        extracted.license_statement == string(source, "license_statement")?,
        "license statement differs from manifest"
    );
    ensure!(
        hash(extracted.text.as_bytes()) == string(source, "abstract_sha256")?,
        "extracted abstract hash differs from manifest"
    );
    let words = extracted.text.split_whitespace().count();
    ensure!(
        (150..=350).contains(&words),
        "abstract outside preregistered 150–350 word range"
    );
    ensure!(
        source["whitespace_words"].as_u64() == Some(words as u64),
        "word count differs from manifest"
    );
    ensure!(
        source["paragraphs"].as_u64() == Some(extracted.paragraphs as u64),
        "paragraph count differs from manifest"
    );
    Ok(extracted)
}

fn cache_path(out_dir: &Path, source: &Value) -> Result<std::path::PathBuf> {
    let relative = Path::new(string(source, "raw_file")?);
    ensure!(
        relative
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "raw_file must be a safe relative path"
    );
    ensure!(relative.starts_with("raw"), "raw_file must be inside raw/");
    Ok(out_dir.join(relative))
}

/// Fetch/cache the manifest's exact publisher XML and atomically write human.jsonl.
/// Hash, license, author, date and abstract checks must all pass before output.
/// A failed build does not replace an existing corpus file.
pub fn build(out_dir: &Path, manifest_path: &Path) -> Result<Value> {
    let manifest_bytes =
        fs::read(manifest_path).with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
    ensure!(
        manifest["schema_version"] == 1,
        "unsupported human source manifest version"
    );
    ensure!(
        manifest["extraction"]["version"] == EXTRACTOR,
        "unsupported abstract extraction version"
    );
    let corpus = string(&manifest, "corpus")?;
    let sources = manifest["sources"]
        .as_array()
        .context("manifest sources must be an array")?;
    ensure!(!sources.is_empty(), "empty source manifest");
    let client = Client::builder()
        .timeout(Duration::from_secs(45))
        .user_agent("unslop-research-pilot/0.2 (+https://github.com/sam0x17/fix-slop)")
        .build()?;
    let mut documents = Vec::new();
    let mut seen_groups = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut seen_authors = HashSet::new();
    let mut downloaded = 0;
    for source in sources {
        let id = string(source, "id")?;
        let doi = string(source, "doi")?;
        ensure!(seen_ids.insert(id), "duplicate source ID {id}");
        ensure!(
            string(source, "group_id")? == format!("doi:{doi}"),
            "source group must match DOI"
        );
        ensure!(
            seen_groups.insert(string(source, "group_id")?),
            "duplicate source DOI"
        );
        ensure!(
            matches!(string(source, "split")?, "train" | "dev" | "test"),
            "invalid source split"
        );
        ensure!(
            string(source, "register")? == "scientific-abstract",
            "unexpected source register"
        );
        string(source, "domain")?;
        let path = cache_path(out_dir, source)?;
        let bytes = if path.exists() {
            fs::read(&path)?
        } else {
            let url = reqwest::Url::parse(string(source, "xml_url")?)?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str() == Some("journals.plos.org")
                    && url.path() == "/plosone/article/file",
                "publisher XML URL must use the official PLOS ONE endpoint"
            );
            ensure!(
                url.query_pairs()
                    .any(|(key, value)| key == "id" && value == doi)
                    && url
                        .query_pairs()
                        .any(|(key, value)| key == "type" && value == "manuscript"),
                "publisher URL does not select this DOI's manuscript"
            );
            let response = client.get(url).send()?.error_for_status()?;
            let bytes = response.bytes()?.to_vec();
            verify_source(source, &bytes).with_context(|| format!("verify downloaded {id}"))?;
            fs::create_dir_all(path.parent().context("raw path has no parent")?)?;
            let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            temp.write_all(&bytes)?;
            temp.as_file().sync_all()?;
            temp.persist(&path)?;
            downloaded += 1;
            bytes
        };
        let extracted = verify_source(source, &bytes).with_context(|| format!("verify {id}"))?;
        for author in &extracted.authors {
            ensure!(
                seen_authors.insert(author.clone()),
                "author name overlaps across pilot articles: {author}"
            );
        }
        documents.push(json!({
            "id": id,
            "text": extracted.text,
            "corpus": corpus,
            "source_kind": "human",
            "domain": source["domain"],
            "register": source["register"],
            "group_id": source["group_id"],
            "split": source["split"],
            "metadata": {
                "source": source,
                "extraction": manifest["extraction"],
                "source_manifest_sha256": hash(&manifest_bytes),
                "collection_protocol": "matched-pilot-v1",
                "comparison_regime": "human original paired with source-conditioned model rewrite"
            }
        }));
    }
    fs::create_dir_all(out_dir)?;
    let output = out_dir.join("human.jsonl");
    let mut temp = tempfile::NamedTempFile::new_in(out_dir)?;
    for document in &documents {
        serde_json::to_writer(&mut temp, document)?;
        temp.write_all(b"\n")?;
    }
    temp.as_file().sync_all()?;
    temp.persist(&output)?;
    let mut splits = serde_json::Map::new();
    for split in ["train", "dev", "test"] {
        splits.insert(
            split.into(),
            json!(documents.iter().filter(|doc| doc["split"] == split).count()),
        );
    }
    Ok(json!({
        "corpus": corpus,
        "documents": documents.len(),
        "source_groups": seen_groups.len(),
        "unique_credited_author_names": seen_authors.len(),
        "whitespace_words": documents.iter().map(|doc| doc["text"].as_str().unwrap().split_whitespace().count()).sum::<usize>(),
        "splits": splits,
        "downloaded_sources": downloaded,
        "output": output,
        "output_sha256": hash(&fs::read(&output)?),
        "manifest_sha256": hash(&manifest_bytes),
        "extractor": EXTRACTOR
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = r#"<?xml version="1.0"?>
<!DOCTYPE article PUBLIC "-//NLM//DTD JATS//EN" "https://example.invalid/jats.dtd">
<article article-type="research-article" xml:lang="en" xmlns:xlink="http://www.w3.org/1999/xlink">
<front><article-meta>
<article-id pub-id-type="doi">10.1371/journal.pone.fixture</article-id>
<title-group><article-title>Fixture <italic>title</italic></article-title></title-group>
<contrib-group><contrib contrib-type="author"><name><surname>Reader</surname><given-names>Casey</given-names></name></contrib></contrib-group>
<pub-date pub-type="epub"><day>2</day><month>1</month><year>2018</year></pub-date>
<permissions><license xlink:href="http://creativecommons.org/licenses/by/4.0/"><license-p>Reuse with attribution.</license-p></license></permissions>
<abstract><sec><title>Background</title><p>We measured <italic>DNA</italic>.
Values varied.</p></sec><sec><title>Results</title><p>We found no effect.</p></sec></abstract>
</article-meta></front><body><p>Never extract the body.</p></body></article>"#;

    #[test]
    fn extracts_inline_text_and_only_abstract_paragraphs() {
        let parsed = extract_xml(ARTICLE.as_bytes()).unwrap();
        assert_eq!(
            parsed.text,
            "We measured DNA. Values varied.\n\nWe found no effect."
        );
        assert_eq!(parsed.paragraphs, 2);
        assert_eq!(parsed.authors, ["Casey Reader"]);
        assert_eq!(parsed.title, "Fixture title");
        assert_eq!(parsed.publication_date, "2018-01-02");
    }

    #[test]
    fn refuses_ambiguous_formula_flattening() {
        let xml = ARTICLE.replace("DNA</italic>", "DNA</italic><sup>2</sup>");
        assert!(
            extract_xml(xml.as_bytes())
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
    }

    #[test]
    fn refuses_wrong_license_or_recent_publication() {
        let xml = ARTICLE.replace("licenses/by/", "licenses/by-nc/");
        assert!(extract_xml(xml.as_bytes()).is_err());
        let xml = ARTICLE.replace("<year>2018</year>", "<year>2025</year>");
        assert!(extract_xml(xml.as_bytes()).is_err());
        assert!(!is_cc_by(
            "https://creativecommons.org.example.com/licenses/by/4.0/"
        ));
    }

    #[test]
    fn rejects_path_traversal() {
        for raw in ["../secret", "/tmp/raw/file.xml", "raw/../../file.xml"] {
            assert!(cache_path(Path::new("data"), &json!({"raw_file": raw})).is_err());
        }
    }

    #[test]
    fn builds_offline_and_leaves_previous_output_on_hash_failure() {
        let dir = tempfile::tempdir().unwrap();
        let text = std::iter::repeat_n("A measured result.", 60)
            .collect::<Vec<_>>()
            .join(" ");
        let xml = ARTICLE.replace("We found no effect.", &text);
        let extracted = extract_xml(xml.as_bytes()).unwrap();
        let mut manifest = json!({
            "schema_version": 1, "corpus": "fixture",
            "extraction": {"version": EXTRACTOR},
            "sources": [{
                "id": "source-1", "doi": extracted.doi, "title": extracted.title,
                "authors": extracted.authors, "publication_date": extracted.publication_date,
                "domain": "biology", "register": "scientific-abstract",
                "group_id": "doi:10.1371/journal.pone.fixture", "split": "train",
                "raw_file": "raw/fixture.xml", "source_sha256": hash(xml.as_bytes()),
                "abstract_sha256": hash(extracted.text.as_bytes()),
                "whitespace_words": extracted.text.split_whitespace().count(), "paragraphs": 2,
                "license_url": "http://creativecommons.org/licenses/by/4.0/",
                "license_statement": "Reuse with attribution."
            }]
        });
        fs::create_dir_all(dir.path().join("raw")).unwrap();
        fs::write(dir.path().join("raw/fixture.xml"), xml).unwrap();
        let manifest_path = dir.path().join("manifest.json");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let summary = build(dir.path(), &manifest_path).unwrap();
        assert_eq!(summary["documents"], 1);
        assert_eq!(summary["downloaded_sources"], 0);
        let output = fs::read(dir.path().join("human.jsonl")).unwrap();
        let document: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(document["text"], extracted.text);
        manifest["sources"][0]["source_sha256"] = json!("bad hash");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(build(dir.path(), &manifest_path).is_err());
        assert_eq!(fs::read(dir.path().join("human.jsonl")).unwrap(), output);
    }
}

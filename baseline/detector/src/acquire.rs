//! Bounded public-source acquisition. Historical dates supply weak supervision.
//!
//! Raw responses and rejected extractions stay in the local acquisition directory.
//! No response is admitted solely because the hosting site permits commercial use.

use crate::dataset::{
    Evidence, Origin, OriginRecord, RECORD_SCHEMA, Rights, Source, write_records,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use regex::Regex;
use reqwest::{Url, blocking::Client};
use roxmltree::{Document, Node, ParsingOptions};
use scraper::{ElementRef, Html, Node as HtmlNode, Selector};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    thread,
    time::Duration,
};

const EXTRACTOR: &str = "slop_ninja_public_origin_acquisition_v1";
const CUTOFF: &str = "2021-12-31T23:59:59Z";
const USER_AGENT: &str =
    "SlopNinjaResearch/0.1 (https://github.com/sam0x17/slop-ninja; bounded corpus pilot)";
const WIKI_RIGHTS: &str = "https://en.wikinews.org/wiki/Wikinews:Copyright";
const PLOS_RIGHTS: &str = "https://plos.org/terms-of-use/";

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Clone)]
pub struct Capture {
    pub bytes: Vec<u8>,
    pub metadata: Value,
}

/// Bounded, hash-checked capture cache shared by public-source importers.
pub struct Collector {
    client: Client,
    root: PathBuf,
    fetched: usize,
    cache_hits: usize,
}

impl Collector {
    pub fn new(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join("raw"))?;
        fs::create_dir_all(root.join("extractions"))?;
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(45))
            .build()?;
        Ok(Self {
            client,
            root: root.into(),
            fetched: 0,
            cache_hits: 0,
        })
    }

    pub fn get(&mut self, url: &Url) -> Result<Capture> {
        let key = digest(url.as_str().as_bytes());
        let meta_path = self.root.join("raw").join(format!("{key}.json"));
        let body_path = self.root.join("raw").join(format!("{key}.body"));
        if meta_path.exists() && body_path.exists() {
            let metadata: Value = serde_json::from_slice(&fs::read(meta_path)?)?;
            let bytes = fs::read(body_path)?;
            ensure!(
                metadata["request_url"] == url.as_str(),
                "cache URL mismatch"
            );
            ensure!(
                metadata["body_sha256"] == digest(&bytes),
                "cache body hash mismatch"
            );
            ensure!(
                metadata["status"]
                    .as_u64()
                    .is_some_and(|s| (200..300).contains(&s)),
                "cached HTTP failure for {url}"
            );
            self.cache_hits += 1;
            return Ok(Capture { bytes, metadata });
        }
        // A sequential stream of at most two requests per second, without retries.
        thread::sleep(Duration::from_millis(500));
        let response = self
            .client
            .get(url.clone())
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        let final_url = response.url().to_string();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let last_modified = response
            .headers()
            .get("last-modified")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut bytes = Vec::new();
        response.take(30_000_001).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 30_000_000,
            "response exceeds bounded acquisition limit"
        );
        let metadata = json!({"request_url":url.as_str(),"final_url":final_url,"retrieved_at":Utc::now().to_rfc3339(),"status":status.as_u16(),"content_type":content_type,"http_last_modified":last_modified,"body_sha256":digest(&bytes),"raw_path":format!("raw/{key}.body")});
        fs::write(&body_path, &bytes)?;
        fs::write(&meta_path, serde_json::to_vec_pretty(&metadata)?)?;
        self.fetched += 1;
        ensure!(status.is_success(), "HTTP {status} for {url}");
        Ok(Capture { bytes, metadata })
    }

    fn wiki(&mut self, pairs: &[(&str, &str)]) -> Result<Capture> {
        let mut url = Url::parse("https://en.wikinews.org/w/api.php")?;
        url.query_pairs_mut()
            .extend_pairs([("format", "json"), ("formatversion", "2")])
            .extend_pairs(pairs.iter().copied());
        self.get(&url)
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {key}"))
}

fn xml_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|n| n.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn xml_child<'a, 'b>(node: Node<'a, 'b>, name: &str) -> Result<Node<'a, 'b>> {
    node.children()
        .find(|n| n.has_tag_name(name))
        .with_context(|| format!("missing JATS {name}"))
}

fn quote_like(text: &str) -> bool {
    static SINGLE_QUOTE: OnceLock<Regex> = OnceLock::new();
    let single_quote = SINGLE_QUOTE.get_or_init(|| {
        Regex::new(r"(?:^|[\s(\[])'[^'\n]{2,}'(?:[\s.,:;!?)\]]|$)").expect("static quote pattern")
    });
    text.chars()
        .any(|c| matches!(c, '"' | '“' | '”' | '«' | '»' | '„' | '‟'))
        || text.contains("''")
        || single_quote.is_match(text)
        || (text.contains('‘') && text.contains('’'))
        || text.to_lowercase().contains("all rights reserved")
}

fn cc_by_version(url: &str) -> Option<&'static str> {
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str() != Some("creativecommons.org")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    match parsed.path().trim_end_matches('/') {
        "/licenses/by/4.0" => Some("CC-BY-4.0"),
        "/licenses/by/3.0" => Some("CC-BY-3.0"),
        "/licenses/by/2.5" => Some("CC-BY-2.5"),
        _ => None,
    }
}

fn plos_extract(raw: &Capture, requested_doi: &str, rights: &Capture) -> Result<Value> {
    let input = std::str::from_utf8(&raw.bytes)?;
    let doc = Document::parse_with_options(
        input,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )?;
    let article = doc.root_element();
    ensure!(
        article.attribute("article-type") == Some("research-article"),
        "not research article"
    );
    ensure!(
        article.attribute(("http://www.w3.org/XML/1998/namespace", "lang")) == Some("en"),
        "not English JATS"
    );
    let meta = xml_child(xml_child(article, "front")?, "article-meta")?;
    let doi = meta
        .children()
        .find(|n| n.has_tag_name("article-id") && n.attribute("pub-id-type") == Some("doi"))
        .map(xml_text)
        .context("missing DOI")?;
    ensure!(doi == requested_doi, "DOI differs from discovery record");
    let title = xml_text(xml_child(xml_child(meta, "title-group")?, "article-title")?);
    let date = meta
        .children()
        .find(|n| n.has_tag_name("pub-date") && n.attribute("pub-type") == Some("epub"))
        .context("no publication date")?;
    let year: i32 = xml_text(xml_child(date, "year")?).parse()?;
    let month: u32 = xml_text(xml_child(date, "month")?).parse()?;
    let day: u32 = xml_text(xml_child(date, "day")?).parse()?;
    let publication_date = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .context("invalid publication date")?
        .to_string();
    ensure!(year == 2019, "outside frozen PLOS publication window");
    let license_node = xml_child(xml_child(meta, "permissions")?, "license")?;
    let license_url = license_node
        .descendants()
        .filter_map(|n| n.attribute(("http://www.w3.org/1999/xlink", "href")))
        .find(|u| cc_by_version(u).is_some())
        .context("no explicit CC BY license")?;
    let license = cc_by_version(license_url).context("unapproved license")?;
    let license_statement = xml_text(license_node);
    let authors: Vec<_> = meta
        .children()
        .filter(|n| n.has_tag_name("contrib-group"))
        .flat_map(|n| n.children())
        .filter(|n| n.has_tag_name("contrib") && n.attribute("contrib-type") == Some("author"))
        .map(|contrib| {
            if let Ok(name) = xml_child(contrib, "name") {
                ["given-names", "surname"]
                    .iter()
                    .filter_map(|tag| xml_child(name, tag).ok())
                    .map(xml_text)
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                xml_child(contrib, "collab")
                    .map(xml_text)
                    .unwrap_or_default()
            }
        })
        .collect();
    ensure!(
        !authors.is_empty() && authors.iter().all(|a| !a.is_empty()),
        "incomplete author attribution"
    );
    let abstract_node = xml_child(meta, "abstract")?;
    // Do not silently rewrite formulae, remove quoted material, or splice around it.
    ensure!(
        !abstract_node.descendants().any(|n| [
            "inline-formula",
            "disp-formula",
            "sup",
            "sub",
            "disp-quote",
            "xref",
            "permissions"
        ]
        .iter()
        .any(|tag| n.has_tag_name(*tag))),
        "abstract contains unsupported formula, quotation, citation or rights markup"
    );
    let mut paragraphs = Vec::new();
    let mut spans = Vec::new();
    for p in abstract_node.descendants().filter(|n| n.has_tag_name("p")) {
        ensure!(
            !p.ancestors()
                .skip(1)
                .take_while(|n| *n != abstract_node)
                .any(|n| n.has_tag_name("p")),
            "nested abstract paragraph"
        );
        let text = xml_text(p);
        ensure!(
            !quote_like(&text),
            "abstract contains possible external quotation"
        );
        if !text.is_empty() {
            paragraphs.push(text);
            spans.push(json!({"raw_utf8_start":p.range().start,"raw_utf8_end":p.range().end}));
        }
    }
    let text = paragraphs.join("\n\n");
    let words = text.split_whitespace().count();
    ensure!(
        (180..=500).contains(&words),
        "abstract outside 180..500 whitespace words"
    );
    let hash = digest(text.as_bytes());
    Ok(json!({
        "schema":EXTRACTOR,"id":format!("plos:{doi}:abstract:{}", &hash[..16]),"source_group":format!("doi:{doi}"),"source":"plos_one","register":"scientific_abstract","label":"human_only","evidence":"historical_publication_proxy","text":text,"text_sha256":hash,"whitespace_words":words,
        "source_url":format!("https://doi.org/{doi}"),"source_id":doi,"source_version":format!("publisher-jats-sha256:{}",raw.metadata["body_sha256"].as_str().unwrap_or("")),"publication_date":publication_date,"revision_date":null,"retrieved_at":raw.metadata["retrieved_at"],"title":title,"authors":authors,"attribution":format!("{}; {title}; https://doi.org/{doi}; {license_url}; abstract paragraphs extracted, markup and section titles omitted",authors.join(", ")),
        "license":license,"license_url":license_url,"license_statement":license_statement,"license_evidence":{"article_jats":raw.metadata,"publisher_policy":rights.metadata},"rights_status":"approved_cc_by_text","commercial_training":true,"model_distribution":true,"text_redistribution":true,
        "origin_notes":"2019 publication is weak human supervision. Publisher JATS downloaded now is content-addressed; no immutable historical revision was supplied. Historical byte identity is unverified. Not a documented human-only workflow or definitive evaluation label.",
        "extraction":{"version":EXTRACTOR,"region":"first article-meta abstract; whole paragraph text only","spans":spans,"changes":["decode XML entities","omit abstract subsection headings","normalize intra-paragraph whitespace","join paragraphs with blank lines"],"quotation_policy":"reject whole abstract for quote marks or quotation markup; no third-party paragraph admitted under blanket site license"},"parent_ids":[]
    }))
}

fn selector(css: &str) -> Selector {
    Selector::parse(css).expect("static CSS selector")
}

fn html_text(node: ElementRef<'_>) -> String {
    node.descendants()
        .filter(|n| {
            !n.ancestors().filter_map(ElementRef::wrap).any(|a| {
                ["script", "style"].contains(&a.value().name())
                    || a.value()
                        .classes()
                        .any(|c| ["published", "dateline", "Z3988", "noprint"].contains(&c))
                    || a.value().attr("id") == Some("publishDate")
            })
        })
        .filter_map(|n| match n.value() {
            HtmlNode::Text(text) => Some::<&str>(text),
            _ => None,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn wiki_extract(
    page: &Value,
    revision_raw: &Capture,
    html_raw: &Capture,
    rights: &Capture,
) -> Result<Value> {
    let revision = &page["revisions"][0];
    let timestamp = string(revision, "timestamp")?;
    ensure!(
        ("2005-09-26T00:00:00Z"..=CUTOFF).contains(&timestamp),
        "revision outside historical CC BY window"
    );
    let revision_id = revision["revid"].as_u64().context("missing revision ID")?;
    let page_id = page["pageid"].as_u64().context("missing page ID")?;
    let wikitext = string(&revision["slots"]["main"], "content")?;
    let folded = wikitext.to_lowercase();
    ensure!(
        ![
            "{{copyvio",
            "{{fair use",
            "{{fairuse",
            "{{copyright",
            "{{wikipedia",
            "{{disputed",
            "{{retracted"
        ]
        .iter()
        .any(|p| folded.contains(p)),
        "article has copyright or provenance exception template"
    );
    ensure!(
        folded.contains("{{publish") || folded.contains("{{date"),
        "not a dated news article"
    );
    let parsed: Value = serde_json::from_slice(&html_raw.bytes)?;
    ensure!(
        parsed["parse"]["revid"].as_u64() == Some(revision_id),
        "rendered revision mismatch"
    );
    let html = string(&parsed["parse"], "text")?;
    let document = Html::parse_fragment(html);
    let container = document
        .select(&selector(".mw-parser-output"))
        .next()
        .context("missing article container")?;
    let mut paragraphs = Vec::new();
    let mut decisions = Vec::new();
    let mut words = 0;
    // Restrict to direct body paragraphs; nested quote/sidebar/table text is excluded.
    // Stop at the source/related-news section, preserving the leading article excerpt.
    for (index, child) in container
        .children()
        .filter_map(ElementRef::wrap)
        .enumerate()
    {
        let tag = child.value().name();
        if ["h2", "h3"].contains(&tag) {
            break;
        }
        if tag != "p" {
            continue;
        }
        let text = html_text(child);
        let count = text.split_whitespace().count();
        let reason = if count < 12 {
            Some("short_or_dateline")
        } else if quote_like(&text)
            || child
                .select(&selector("q, blockquote, sup, .reference"))
                .next()
                .is_some()
        {
            Some("possible_external_quotation_or_reference")
        } else if child
            .value()
            .attr("class")
            .is_some_and(|c| c.contains("dablink") || c.contains("noprint"))
        {
            Some("nonarticle_paragraph")
        } else if words + count > 500 {
            Some("excerpt_word_limit")
        } else {
            None
        };
        decisions.push(json!({"direct_child_index":index,"paragraph_sha256":digest(text.as_bytes()),"whitespace_words":count,"decision":reason.unwrap_or("retained")}));
        if reason == Some("excerpt_word_limit") {
            break;
        }
        if reason.is_none() {
            words += count;
            paragraphs.push(text);
        }
    }
    ensure!(
        (180..=500).contains(&words),
        "news excerpt outside 180..500 words after exclusion"
    );
    let text = paragraphs.join("\n\n");
    let hash = digest(text.as_bytes());
    let source_url = format!("https://en.wikinews.org/w/index.php?oldid={revision_id}");
    let title = string(page, "title")?;
    Ok(json!({
        "schema":EXTRACTOR,"id":format!("wikinews:{page_id}:{revision_id}:{}", &hash[..16]),"source_group":format!("wikinews:{page_id}"),"source":"wikinews","register":"news_reporting","label":"human_only","evidence":"historical_publication_proxy","text":text,"text_sha256":hash,"whitespace_words":words,
        "source_url":source_url,"source_id":page_id.to_string(),"source_version":revision_id.to_string(),"publication_date":null,"revision_date":timestamp,"retrieved_at":revision_raw.metadata["retrieved_at"],"title":title,"authors":["Wikinews contributors"],"attribution":format!("Wikinews; {title}; {source_url}; CC BY 2.5; paragraph excerpt with possible quotations excluded; contributor history https://en.wikinews.org/w/index.php?curid={page_id}&action=history"),
        "license":"CC-BY-2.5","license_url":"https://creativecommons.org/licenses/by/2.5/","license_statement":"Wikinews copyright policy licenses text published after September 25, 2005 and before December 16, 2024 under CC BY 2.5 and permits attribution to Wikinews.","license_evidence":{"publisher_policy":rights.metadata,"revision_response":revision_raw.metadata,"rendered_revision_response":html_raw.metadata},"rights_status":"approved_cc_by_text","commercial_training":true,"model_distribution":true,"text_redistribution":true,
        "origin_notes":"Exact pre-2022 revision is weak historical human supervision, not a documented human-only workflow. Contributors have collective attribution; the last editor is not treated as sole author. Templates were rendered at retrieval time; only direct prose paragraphs from the revision are extracted.",
        "extraction":{"version":EXTRACTOR,"region":"direct article-body paragraphs before first subsection","paragraph_decisions":decisions,"changes":["render exact revision with MediaWiki","decode HTML entities","omit marked publisher dateline and hidden metadata nodes","normalize intra-paragraph whitespace","exclude quote-like, reference and nonarticle paragraphs","cap excerpt at 500 whitespace words"]},"parent_ids":[]
    }))
}

fn append_json(file: &mut fs::File, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    writeln!(file)?;
    file.flush()?;
    Ok(())
}

fn origin_record(value: &Value, root: &Path) -> Result<OriginRecord> {
    let is_plos = value["source"] == "plos_one";
    let raw = if is_plos {
        &value["license_evidence"]["article_jats"]
    } else {
        &value["license_evidence"]["revision_response"]
    };
    let license_evidence = if is_plos {
        raw
    } else {
        &value["license_evidence"]["publisher_policy"]
    };
    let record = OriginRecord {
        schema: RECORD_SCHEMA.into(),
        id: string(value, "id")?.into(),
        source_group: string(value, "source_group")?.into(),
        split: None,
        origin: Origin::HumanOnly,
        evidence: Evidence::HistoricalProxy,
        evidence_notes: format!(
            "{} Register: {}. Historical revision timestamp: {}. Detailed acquisition metadata: extractions/{}.json",
            string(value, "origin_notes")?,
            string(value, "register")?,
            value["revision_date"].as_str().unwrap_or("not supplied"),
            string(value, "text_sha256")?
        ),
        text: string(value, "text")?.into(),
        text_sha256: string(value, "text_sha256")?.into(),
        source: Source {
            collection: string(value, "source")?.into(),
            url: string(value, "source_url")?.into(),
            version: string(value, "source_version")?.into(),
            published_at: value["publication_date"].as_str().map(str::to_owned),
            author_ids: value["authors"]
                .as_array()
                .context("missing authors")?
                .iter()
                .map(|v| v.as_str().context("invalid author").map(str::to_owned))
                .collect::<Result<Vec<_>>>()?,
            raw_path: root
                .canonicalize()?
                .join(string(raw, "raw_path")?)
                .to_string_lossy()
                .into_owned(),
            raw_sha256: string(raw, "body_sha256")?.into(),
            extraction: EXTRACTOR.into(),
        },
        rights: Rights {
            license: string(value, "license")?.into(),
            evidence_url: string(license_evidence, "request_url")?.into(),
            evidence_sha256: string(license_evidence, "body_sha256")?.into(),
            attribution: string(value, "attribution")?.into(),
            commercial_training: true,
            model_release: true,
            external_evaluation: true,
            redistribute_text: true,
            share_alike: None,
        },
        parent_id: None,
        generation: None,
    };
    record.validate()?;
    Ok(record)
}

/// Publish provenance and attribution without corpus text or local file paths.
pub fn export_source_manifest(input: &Path, output: &Path) -> Result<()> {
    let records = crate::dataset::read_records(input)?;
    let mut bytes = Vec::new();
    for record in records {
        ensure!(
            record.parent_id.is_none() && record.evidence == Evidence::HistoricalProxy,
            "source manifest expects historical source roots"
        );
        let value = json!({"schema":"slop-ninja-public-source-manifest-v1","id":record.id,"source_group":record.source_group,"collection":record.source.collection,"source_url":record.source.url,"source_version":record.source.version,"published_at":record.source.published_at,"attributed_authors":record.source.author_ids,"raw_sha256":record.source.raw_sha256,"text_sha256":record.text_sha256,"extraction_version":record.source.extraction,"origin_evidence":record.evidence,"origin_notes":record.evidence_notes,"license":record.rights.license,"license_evidence_url":record.rights.evidence_url,"license_evidence_sha256":record.rights.evidence_sha256,"attribution":record.rights.attribution,"commercial_training":record.rights.commercial_training,"model_release":record.rights.model_release,"external_evaluation":record.rights.external_evaluation,"redistribute_text":record.rights.redistribute_text});
        let mut value = value;
        if let Some(obligations) = record.rights.share_alike {
            value["share_alike"] = serde_json::to_value(obligations)?;
        }
        serde_json::to_writer(&mut bytes, &value)?;
        bytes.push(b'\n');
    }
    if output.exists() {
        ensure!(
            fs::read(output)? == bytes,
            "Existing public manifest differs"
        );
    } else {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(output)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(())
}

fn record_attempt(
    root: &Path,
    attempts: &mut fs::File,
    attempt: (&str, &str, Result<Value>),
    counts: &mut BTreeMap<String, usize>,
    accepted: &mut Vec<Value>,
    seen: &mut BTreeSet<String>,
) -> Result<()> {
    let (source, id, result) = attempt;
    match result {
        Ok(value) => {
            let text_hash = string(&value, "text_sha256")?.to_owned();
            if !seen.insert(text_hash.clone()) {
                *counts
                    .entry(format!("{source}:duplicate_text"))
                    .or_default() += 1;
                append_json(
                    attempts,
                    &json!({"source":source,"source_id":id,"status":"excluded","reason":"duplicate_text","text_sha256":text_hash}),
                )?;
            } else {
                fs::write(
                    root.join("extractions").join(format!("{text_hash}.json")),
                    serde_json::to_vec_pretty(&value)?,
                )?;
                append_json(
                    attempts,
                    &json!({"source":source,"source_id":id,"status":"admitted_weak_human_proxy","id":value["id"],"text_sha256":text_hash,"whitespace_words":value["whitespace_words"]}),
                )?;
                accepted.push(value);
                *counts.entry(format!("{source}:admitted")).or_default() += 1;
                if accepted.len().is_multiple_of(20) {
                    eprintln!("acquisition: {} accepted source works", accepted.len());
                }
            }
        }
        Err(error) => {
            let reason = error.to_string();
            *counts.entry(format!("{source}:{reason}")).or_default() += 1;
            append_json(
                attempts,
                &json!({"source":source,"source_id":id,"status":"excluded","reason":reason}),
            )?;
        }
    }
    Ok(())
}

/// Acquire a frozen, bounded pilot. Existing captures are hash-checked and reused.
/// `human-candidates.jsonl` uses the raw acquisition schema, before dataset import.
pub fn acquire(output: &Path, plos_target: usize, wikinews_target: usize) -> Result<Value> {
    acquire_from(output, plos_target, wikinews_target, 0, "A")
}

/// Use recorded discovery offsets for a fresh bounded collection. This changes
/// discovery only; item-level rights and extraction checks remain identical.
pub fn acquire_from(
    output: &Path,
    plos_target: usize,
    wikinews_target: usize,
    plos_start: usize,
    wikinews_from: &str,
) -> Result<Value> {
    ensure!(
        plos_target <= 100 && wikinews_target <= 100,
        "initial source review is capped at 100 admitted works per source"
    );
    ensure!(plos_start <= 100_000, "PLOS discovery offset exceeds bound");
    ensure!(
        !wikinews_from.trim().is_empty() && wikinews_from.len() <= 200,
        "Invalid Wikinews discovery start"
    );
    let mut collector = Collector::new(output)?;
    let mut config = json!({"schema":EXTRACTOR,"plos_target":plos_target,"wikinews_target":wikinews_target,"plos_candidate_limit":350,"wikinews_candidate_limit":600,"plos_publication_year":2019,"wikinews_revision_cutoff":CUTOFF,"whitespace_word_range":[180,500],"user_agent":USER_AGENT,"request_interval_ms":500,"retries":0});
    if plos_start != 0 || wikinews_from != "A" {
        config["discovery_offsets"] =
            json!({"plos_start":plos_start,"wikinews_from":wikinews_from});
    }
    let config_path = output.join("config.json");
    if config_path.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&fs::read(&config_path)?)? == config,
            "acquisition configuration changed; use a new directory"
        );
    } else {
        fs::write(config_path, serde_json::to_vec_pretty(&config)?)?;
    }
    let mut attempts = fs::File::create(output.join("attempts.jsonl"))?;
    let mut accepted = Vec::new();
    let mut counts = BTreeMap::new();
    let mut seen = BTreeSet::new();

    if plos_target > 0 {
        let rights = collector.get(&Url::parse(PLOS_RIGHTS)?)?;
        ensure!(
            String::from_utf8_lossy(&rights.bytes).contains("Creative Commons"),
            "PLOS rights policy changed or was not retrieved"
        );
        let mut url = Url::parse("https://api.plos.org/search")?;
        url.query_pairs_mut().extend_pairs([("q","publication_date:[2019-01-01T00:00:00Z TO 2019-12-31T23:59:59Z] AND doc_type:full AND journal_key:PLoSONE"),("fl","id,title,publication_date"),("rows","350"),("sort","id asc"),("wt","json")]);
        if plos_start != 0 {
            url.query_pairs_mut()
                .append_pair("start", &plos_start.to_string());
        }
        let listing = collector.get(&url)?;
        let values: Value = serde_json::from_slice(&listing.bytes)?;
        for doc in values["response"]["docs"]
            .as_array()
            .context("missing PLOS results")?
        {
            if counts.get("plos_one:admitted").copied().unwrap_or(0) >= plos_target {
                break;
            }
            let doi = string(doc, "id")?;
            let mut url = Url::parse("https://journals.plos.org/plosone/article/file")?;
            url.query_pairs_mut()
                .extend_pairs([("id", doi), ("type", "manuscript")]);
            let result = collector
                .get(&url)
                .and_then(|raw| plos_extract(&raw, doi, &rights));
            record_attempt(
                output,
                &mut attempts,
                ("plos_one", doi, result),
                &mut counts,
                &mut accepted,
                &mut seen,
            )?;
        }
    }

    if wikinews_target > 0 {
        let rights = collector.get(&Url::parse(WIKI_RIGHTS)?)?;
        ensure!(
            String::from_utf8_lossy(&rights.bytes).contains("2.5"),
            "Wikinews rights policy changed or was not retrieved"
        );
        let mut continuation = wikinews_from.to_owned();
        let mut candidates = 0;
        while candidates < 600
            && counts.get("wikinews:admitted").copied().unwrap_or(0) < wikinews_target
        {
            let listing = collector.wiki(&[
                ("action", "query"),
                ("list", "allpages"),
                ("apnamespace", "0"),
                ("apfilterredir", "nonredirects"),
                ("aplimit", "500"),
                ("apfrom", &continuation),
            ])?;
            let values: Value = serde_json::from_slice(&listing.bytes)?;
            let pages = values["query"]["allpages"]
                .as_array()
                .context("missing Wikinews discovery pages")?;
            for page in pages {
                if candidates >= 600
                    || counts.get("wikinews:admitted").copied().unwrap_or(0) >= wikinews_target
                {
                    break;
                }
                candidates += 1;
                let id = page["pageid"]
                    .as_u64()
                    .context("missing discovery page ID")?
                    .to_string();
                let result = (|| -> Result<Value> {
                    let revisions = collector.wiki(&[
                        ("action", "query"),
                        ("prop", "revisions"),
                        ("pageids", &id),
                        ("rvprop", "ids|timestamp|content"),
                        ("rvslots", "main"),
                        ("rvlimit", "1"),
                        ("rvstart", CUTOFF),
                        ("rvdir", "older"),
                    ])?;
                    let parsed: Value = serde_json::from_slice(&revisions.bytes)?;
                    let page = &parsed["query"]["pages"][0];
                    let revid = page["revisions"][0]["revid"]
                        .as_u64()
                        .context("no historical revision")?
                        .to_string();
                    let rendered = collector.wiki(&[
                        ("action", "parse"),
                        ("oldid", &revid),
                        ("prop", "text|revid"),
                        ("disablelimitreport", "1"),
                        ("disableeditsection", "1"),
                    ])?;
                    wiki_extract(page, &revisions, &rendered, &rights)
                })();
                record_attempt(
                    output,
                    &mut attempts,
                    ("wikinews", &id, result),
                    &mut counts,
                    &mut accepted,
                    &mut seen,
                )?;
            }
            let Some(next) = values["continue"]["apcontinue"].as_str() else {
                break;
            };
            continuation = next.to_owned();
        }
    }

    let mut records = fs::File::create(output.join("human-candidates.jsonl"))?;
    for value in &accepted {
        append_json(&mut records, value)?;
    }
    let origin_records = accepted
        .iter()
        .map(|v| origin_record(v, output))
        .collect::<Result<Vec<_>>>()?;
    let origin_path = output.join("human-records.jsonl");
    if origin_path.exists() {
        let existing = crate::dataset::read_records(&origin_path)?;
        ensure!(
            serde_json::to_value(&existing)? == serde_json::to_value(&origin_records)?,
            "existing imported records differ; use a new acquisition directory"
        );
    } else {
        write_records(&origin_path, &origin_records)?;
    }
    let summary = json!({"schema":EXTRACTOR,"completed_at":Utc::now().to_rfc3339(),"counts":counts,"admitted_source_works":accepted.len(),"new_http_captures":collector.fetched,"reused_http_captures":collector.cache_hits,"record_file":"human-candidates.jsonl","record_file_sha256":digest(&fs::read(output.join("human-candidates.jsonl"))?),"limitations":["All accepted human labels are weak historical proxies, not documented human-only workflows.","PLOS publisher XML has a 2019 publication date but no immutable historical byte revision; current content-addressed bytes retained.","Wikinews uses exact pre-2022 revisions, but templates render at retrieval time; source prose filtering excludes nested material and possible quotations.","Two registers and deterministic source discovery are a bounded acquisition pilot, not representative detector evaluation.","Quote-marker filtering is conservative screening, not a complete third-party-rights audit; source review is still required."]});
    fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn licenses_and_quotations_fail_closed() {
        assert_eq!(
            cc_by_version("https://creativecommons.org/licenses/by/4.0/"),
            Some("CC-BY-4.0")
        );
        for url in [
            "https://creativecommons.org.evil.test/licenses/by/4.0/",
            "https://creativecommons.org/licenses/by-nc/4.0/",
            "https://creativecommons.org/licenses/by-nd/4.0/",
            "https://creativecommons.org/licenses/by-sa/4.0/",
        ] {
            assert_eq!(cc_by_version(url), None);
        }
        assert!(quote_like(
            "The speaker said “please use a different excerpt.”"
        ));
        assert!(quote_like("A ‘quotation’ survives HTML entity decoding."));
        assert!(quote_like("She wrote 'a complete borrowed sentence'."));
        assert!(!quote_like(
            "An author's name doesn't establish who wrote each sentence."
        ));
    }

    #[test]
    fn dateline_in_lead_paragraph_is_not_training_text() {
        let document = Html::parse_fragment(
            "<p><strong class='published'>Thursday, May 16, 2019</strong> The report begins here <a href='/wiki/Somewhere'>and continues</a>.</p>",
        );
        let paragraph = document.select(&selector("p")).next().unwrap();
        assert_eq!(
            html_text(paragraph),
            "The report begins here and continues."
        );
    }
}

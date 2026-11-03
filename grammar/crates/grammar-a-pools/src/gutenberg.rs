//! Project Gutenberg adapter: per-book plain text plus its RDF record.
//!
//! The runner downloads `pgN.txt` and `N.rdf` and writes one manifest row
//! per book. Excerpts use the established book rule byte for byte; the
//! stored text is the exact UTF-8 slice of the downloaded file.
use crate::{
    adapter::Run,
    io,
    record::{self, Doc},
    text::{self, Excerpt},
};
use anyhow::{Context, Result};
use roxmltree::{Document, Node};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

const PG: &str = "http://www.gutenberg.org/2009/pgterms/";
const DC: &str = "http://purl.org/dc/terms/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const PG_RIGHTS: &str = "Public domain in the USA.";
const MAX_BOOK_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub pg_id: u64,
    pub title: String,
    pub creators: Vec<String>,
    pub creator_death_years: Vec<Option<i64>>,
    pub other_roles: Vec<String>,
    pub languages: Vec<String>,
    pub rights: String,
    pub issued: String,
    pub types: Vec<String>,
    pub subjects: Vec<String>,
}

fn value_of(n: Node) -> Option<String> {
    n.descendants()
        .find(|d| d.has_tag_name((RDF, "value")))
        .and_then(|v| v.text())
        .map(|t| t.trim().to_owned())
}

pub fn parse_rdf(xml: &str) -> Result<Catalog> {
    let doc = Document::parse(xml).context("RDF XML")?;
    let ebook = doc
        .descendants()
        .find(|n| n.has_tag_name((PG, "ebook")))
        .context("pgterms:ebook")?;
    let about = ebook.attribute((RDF, "about")).context("ebook rdf:about")?;
    let pg_id = about.trim_start_matches("ebooks/").parse().context("ebook id")?;
    let child_text = |name: &str| {
        ebook
            .children()
            .find(|c| c.has_tag_name((DC, name)))
            .and_then(|c| c.text())
            .map(|t| t.trim().to_owned())
    };
    let mut creators = Vec::new();
    let mut deaths = Vec::new();
    let mut other = Vec::new();
    for c in ebook.children().filter(|c| c.is_element()) {
        let agent_name = || {
            c.descendants()
                .find(|d| d.has_tag_name((PG, "name")))
                .and_then(|d| d.text())
                .map(|t| t.trim().to_owned())
        };
        if c.has_tag_name((DC, "creator")) {
            if let Some(n) = agent_name() {
                creators.push(n);
                deaths.push(
                    c.descendants()
                        .find(|d| d.has_tag_name((PG, "deathdate")))
                        .and_then(|d| d.text())
                        .and_then(|t| t.trim().parse().ok()),
                );
            }
        } else if c.tag_name().namespace() == Some("http://id.loc.gov/vocabulary/relators/") {
            other.push(c.tag_name().name().to_owned());
        }
    }
    let values = |name: &str| {
        ebook
            .children()
            .filter(|c| c.has_tag_name((DC, name)))
            .filter_map(value_of)
            .collect::<Vec<_>>()
    };
    Ok(Catalog {
        pg_id,
        title: child_text("title").unwrap_or_default().replace(['\r', '\n'], " "),
        creators,
        creator_death_years: deaths,
        other_roles: other,
        languages: values("language"),
        rights: child_text("rights").unwrap_or_default(),
        issued: child_text("issued").unwrap_or_default(),
        types: values("type"),
        subjects: values("subject"),
    })
}

/// Why a catalog record cannot supply a single-author public-domain English
/// text, if it cannot.
pub fn catalog_screen(c: &Catalog) -> Option<&'static str> {
    if c.rights != PG_RIGHTS {
        return Some("rights_not_public_domain_in_usa");
    }
    if c.languages != ["en"] {
        return Some("not_english_only");
    }
    if !c.types.iter().any(|t| t == "Text") {
        return Some("not_text_type");
    }
    if c.creators.len() != 1 {
        return Some("not_single_creator");
    }
    let l = c.creators[0].to_lowercase();
    if ["various", "anonymous", "unknown"].iter().any(|w| l.contains(w)) {
        return Some("collective_or_unknown_creator");
    }
    if c.title.trim().is_empty() {
        return Some("no_title");
    }
    if !record::before_cutoff(&c.issued) {
        return Some("pg_release_not_before_2022_11_30");
    }
    None
}

pub struct Inventory(pub BTreeMap<u64, Value>);

impl Inventory {
    /// Optional read-only catalog inventory (stratum and aliases).
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut m = BTreeMap::new();
        if let Some(p) = path {
            for r in io::jsonl_rows(p)? {
                let r = r?;
                if let Some(id) = r["pg_id"].as_u64() {
                    m.insert(id, r);
                }
            }
        }
        Ok(Inventory(m))
    }
}

/// Which book excerpt rule the adapter applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExcerptRule {
    /// `PG_EXCERPT_METHOD`, kept to reproduce earlier rows byte for byte.
    V1,
    /// `PG_EXCERPT_METHOD_V2` (quotation break) plus the English screen.
    V2,
}

impl std::str::FromStr for ExcerptRule {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "v1" => Ok(ExcerptRule::V1),
            "v2" => Ok(ExcerptRule::V2),
            _ => Err(format!("unknown excerpt rule {s} (v1 or v2)")),
        }
    }
}

/// RDF subject terms that mark criticism, biography or compiled material,
/// where quoted spans are mostly other writers' words even inside the
/// fiction stratum.
const QUOTING_SUBJECTS: &[&str] = &[
    "history and criticism", "criticism and interpretation", "biography", "correspondence", "quotations", "anthologies",
    "literary collections", "addresses, essays, lectures",
];

/// Whether inline quoted spans count as quotation material: every stratum
/// except fiction, whose dialogue is the author's own prose, and fiction
/// works whose RDF subjects mark criticism, biography or compilations.
pub fn inline_quotes_for(domain: &str, subjects: &[String]) -> bool {
    domain != "fiction" || subjects.iter().any(|s| {
        let l = s.to_lowercase();
        QUOTING_SUBJECTS.iter().any(|q| l.contains(q))
    })
}

pub fn adapt(manifest: &Path, fractions: &[usize], inventory: &Inventory, rule: ExcerptRule, run: &mut Run) -> Result<()> {
    let base = manifest.parent().unwrap_or(Path::new("."));
    for row in io::jsonl_rows(manifest)? {
        if run.full() {
            break;
        }
        let row = row?;
        run.units_seen += 1;
        let pg = row["pg_id"].as_u64().context("manifest pg_id")?;
        let unit = format!("pg:{pg}");
        let resolve = |k: &str| -> Option<std::path::PathBuf> {
            row[k].as_str().map(|p| {
                let p = Path::new(p);
                if p.is_absolute() { p.to_owned() } else { base.join(p) }
            })
        };
        let (Some(tp), Some(rp)) = (resolve("text_path"), resolve("rdf_path")) else {
            run.reject(&unit, "manifest_missing_paths")?;
            continue;
        };
        // Metadata first: the RDF decides rights, family and bucket before
        // the book text is read at all.
        let Ok(rdf_bytes) = io::read_all(&rp, 8 * 1024 * 1024) else {
            run.reject(&unit, "download_unreadable")?;
            continue;
        };
        let rdf_sha = text::sha_hex(&rdf_bytes);
        if let Some(expected) = row["rdf_sha256"].as_str() {
            anyhow::ensure!(expected == rdf_sha, "{unit}: rdf_sha256 changed since download");
        }
        let Ok(rdf) = std::str::from_utf8(&rdf_bytes) else {
            run.reject(&unit, "rdf_not_utf8")?;
            continue;
        };
        let cat = match parse_rdf(rdf) {
            Ok(c) if c.pg_id == pg => c,
            Ok(_) => {
                run.reject(&unit, "rdf_id_mismatch")?;
                continue;
            }
            Err(_) => {
                run.reject(&unit, "rdf_unparsed")?;
                continue;
            }
        };
        if let Some(r) = catalog_screen(&cat) {
            run.reject(&unit, r)?;
            continue;
        }
        let author = text::author_key(&cat.creators[0]);
        let work = text::title_key(&cat.title);
        if author.is_empty() || work.is_empty() {
            run.reject(&unit, "empty_family_key")?;
            continue;
        }
        let family = format!("author_work_v2:{author}:{work}");
        if !run.sampled(&family) {
            continue;
        }
        let Ok(text_bytes) = io::read_all(&tp, MAX_BOOK_BYTES) else {
            run.reject(&unit, "download_unreadable")?;
            continue;
        };
        let text_sha = text::sha_hex(&text_bytes);
        if let Some(expected) = row["text_sha256"].as_str() {
            anyhow::ensure!(expected == text_sha, "{unit}: text_sha256 changed since download");
        }
        let Ok(book) = std::str::from_utf8(&text_bytes) else {
            run.reject(&unit, "text_not_utf8")?;
            continue;
        };
        let inv = inventory.0.get(&pg);
        let mut aliases = vec![unit.clone(), family.clone()];
        aliases.extend(text::title_aliases(&cat.title));
        if let Some(a) = inv.and_then(|v| v["aliases"].as_array()) {
            aliases.extend(a.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        aliases.sort();
        aliases.dedup();
        let domain = inv.and_then(|v| v["stratum"].as_str()).unwrap_or("literature").to_owned();
        let rights = {
            let mut r = record::rights(
                "public-domain-us",
                format!("{} — {}; Project Gutenberg eBook #{pg}, https://www.gutenberg.org/ebooks/{pg}", cat.creators[0], cat.title),
                &format!("https://www.gutenberg.org/ebooks/{pg}.rdf"),
                "Retained hash-bound Project Gutenberg RDF labels the underlying English work public domain in the USA; this does not establish rights elsewhere.",
            );
            r["jurisdiction"] = json!("US");
            r["catalog_statement"] = json!(cat.rights);
            r["evidence_sha256"] = json!(rdf_sha);
            r["license_url"] = json!("https://www.gutenberg.org/policy/license.html");
            r["permission_url"] = json!("https://www.gutenberg.org/policy/permission.html");
            r["conditions"] = json!("Preserve PG notices and applicable trademark/license terms in PG-branded copies; the excerpt lies inside the PG body markers.");
            r
        };
        for &fraction in fractions {
            let id = format!("pool-pg{pg}-p{fraction}");
            let inline = inline_quotes_for(&domain, &cat.subjects);
            let (start, end, restarts) = match rule {
                ExcerptRule::V1 => match text::pg_excerpt(book, fraction) {
                    Ok((s, e)) => (s, e, None),
                    Err(r) => {
                        run.reject(&id, r)?;
                        continue;
                    }
                },
                ExcerptRule::V2 => match text::pg_excerpt_v2(book, fraction, inline) {
                    Ok(x) => (x.start, x.end, Some(x.quotation_restarts)),
                    Err(r) => {
                        run.reject(&id, r)?;
                        continue;
                    }
                },
            };
            let (latin, stop) = (text::latin_letter_share(&book[start..end]), text::english_stopword_share(&book[start..end]));
            if rule == ExcerptRule::V2 {
                if let Some(r) = text::english_screen(&book[start..end]) {
                    run.reject(&id, r)?;
                    continue;
                }
            }
            let ex = Excerpt {
                text: book[start..end].to_owned(),
                start,
                end,
                words: text::words(&book[start..end]),
                complete_unit: false,
            };
            let doc = Doc {
                id,
                collection: "project_gutenberg_historical",
                domain: &domain,
                source_group: family.clone(),
                writer_key: Some(format!("author:{author}")),
                evidence: "historical_public_domain_author_proxy",
                excerpt: &ex,
                extraction_method: if rule == ExcerptRule::V1 { text::PG_EXCERPT_METHOD } else { text::PG_EXCERPT_METHOD_V2 },
                unit: json!({"kind":"pg_plain_text","span_basis":"byte_offsets_in_downloaded_file",
                             "byte_span":[start,end],"fraction_percent":fraction,"file_sha256":text_sha,
                             "file_bytes":text_bytes.len(),"quotation_restarts":restarts,
                             "quotation_rule": if rule == ExcerptRule::V1 { json!(null) } else if inline { json!("indented_blocks_and_inline_quoted_spans_of_8_words") } else { json!("indented_blocks_only_fiction_dialogue_kept") },
                             "language_screen": {"latin_letter_share": (latin * 1e4).round() / 1e4, "english_stopword_share": (stop * 1e4).round() / 1e4,
                                                 "applied": rule == ExcerptRule::V2, "min_latin_letter_share": text::MIN_LATIN_LETTER_SHARE,
                                                 "min_english_stopword_share": text::MIN_ENGLISH_STOPWORD_SHARE}}),
                provenance: json!({"pg_id":pg,"title":cat.title,"catalog_author":cat.creators[0],
                                   "author_key":author,"creator_death_years":cat.creator_death_years,
                                   "other_relator_roles":cat.other_roles,"subjects":cat.subjects,
                                   "pg_ebook_release_date":cat.issued,"rdf_sha256":rdf_sha,
                                   "source_url":row["text_url"],"rdf_url":row["rdf_url"],
                                   "retrieved_utc":row["retrieved_utc"],"inventory_stratum":inv.map(|v| v["stratum"].clone()),
                                   "dates_note":"PG release dates are ebook metadata, never publication or composition dates."}),
                date: Some(cat.issued.clone()),
                date_basis: "pg_ebook_release_date_metadata_only",
                cutoff_policy: "public_domain_historical",
                rights: rights.clone(),
                aliases: aliases.clone(),
            };
            run.accept(&doc.id.clone(), &doc)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn rdf(id: u64, title: &str, creator: &str, rights: &str, lang: &str, issued: &str) -> String {
        format!(r#"<?xml version="1.0" encoding="utf-8"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:pgterms="http://www.gutenberg.org/2009/pgterms/" xmlns:marcrel="http://id.loc.gov/vocabulary/relators/">
<pgterms:ebook rdf:about="ebooks/{id}">
<dcterms:title>{title}</dcterms:title>
<dcterms:creator><pgterms:agent rdf:about="2009/agents/1"><pgterms:name>{creator}</pgterms:name><pgterms:deathdate>1900</pgterms:deathdate></pgterms:agent></dcterms:creator>
<marcrel:ill><pgterms:agent><pgterms:name>Someone</pgterms:name></pgterms:agent></marcrel:ill>
<dcterms:language><rdf:Description><rdf:value rdf:datatype="http://purl.org/dc/terms/RFC4646">{lang}</rdf:value></rdf:Description></dcterms:language>
<dcterms:rights>{rights}</dcterms:rights>
<dcterms:issued rdf:datatype="http://www.w3.org/2001/XMLSchema#date">{issued}</dcterms:issued>
<dcterms:type><rdf:Description><rdf:value>Text</rdf:value></rdf:Description></dcterms:type>
</pgterms:ebook></rdf:RDF>"#)
    }

    fn book() -> String {
        let body = (0..500).map(|i| format!("Sentence number {i} goes here. ")).collect::<String>();
        format!("The Project Gutenberg eBook\r\n*** START OF THE PROJECT GUTENBERG EBOOK SYNTH ***\r\n{body}\r\n*** END OF THE PROJECT GUTENBERG EBOOK SYNTH ***\r\n")
    }

    #[test]
    fn inline_quote_rule_follows_stratum_and_subjects() {
        assert!(!inline_quotes_for("fiction", &["Love stories".into()]));
        assert!(inline_quotes_for("fiction", &["Didactic fiction, English -- History and criticism".into()]));
        assert!(inline_quotes_for("travel", &[]));
    }

    #[test]
    fn rdf_parse_and_screen() {
        let c = parse_rdf(&rdf(7, "The Synthetic Tale", "Doe, Jane", PG_RIGHTS, "en", "2004-01-01")).unwrap();
        assert_eq!((c.pg_id, c.creators.clone(), c.languages.clone()), (7, vec!["Doe, Jane".to_owned()], vec!["en".to_owned()]));
        assert_eq!(c.other_roles, vec!["ill"]);
        assert_eq!(catalog_screen(&c), None);
        let c = parse_rdf(&rdf(7, "T", "Doe, Jane", "Copyrighted. Read the copyright notice inside this book for details.", "en", "2004-01-01")).unwrap();
        assert_eq!(catalog_screen(&c), Some("rights_not_public_domain_in_usa"));
        let c = parse_rdf(&rdf(7, "T", "Various", PG_RIGHTS, "en", "2004-01-01")).unwrap();
        assert_eq!(catalog_screen(&c), Some("collective_or_unknown_creator"));
        let c = parse_rdf(&rdf(7, "T", "Doe, Jane", PG_RIGHTS, "fr", "2004-01-01")).unwrap();
        assert_eq!(catalog_screen(&c), Some("not_english_only"));
        let c = parse_rdf(&rdf(7, "T", "Doe, Jane", PG_RIGHTS, "en", "2023-05-01")).unwrap();
        assert_eq!(catalog_screen(&c), Some("pg_release_not_before_2022_11_30"));
    }

    #[test]
    fn adapter_keeps_exact_bytes_and_family_keys() {
        let d = tempfile::tempdir().unwrap();
        let b = book();
        std::fs::write(d.path().join("pg7.txt"), &b).unwrap();
        std::fs::write(d.path().join("7.rdf"), rdf(7, "The Synthetic Tale: A Story", "Doe, Jane (Jane Q.)", PG_RIGHTS, "en", "2004-01-01")).unwrap();
        let m = d.path().join("manifest.jsonl");
        std::fs::write(&m, format!("{}\n", json!({"pg_id":7,"text_path":"pg7.txt","rdf_path":"7.rdf","text_sha256":text::sha_hex(b.as_bytes())}))).unwrap();
        let out = d.path().join("out");
        let mut run = Run::start(&out, "gutenberg", "project_gutenberg_historical", json!({}), None).unwrap();
        adapt(&m, &[20, 60], &Inventory::load(None).unwrap(), ExcerptRule::V1, &mut run).unwrap();
        let rep = run.finish(json!({})).unwrap();
        assert_eq!(rep["candidates"], 2);
        let rows: Vec<Value> = io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().collect::<Result<_>>().unwrap();
        let r = &rows[0];
        assert_eq!(r["source_group"], "author_work_v2:jane_doe:synthetic_tale");
        let span = r["extraction"]["unit"]["byte_span"].as_array().unwrap();
        let (s, e) = (span[0].as_u64().unwrap() as usize, span[1].as_u64().unwrap() as usize);
        assert_eq!(r["text"].as_str().unwrap(), &b[s..e]);
        assert!(r["aliases"].as_array().unwrap().iter().any(|a| a == "pg:7"));
        assert!(r["aliases"].as_array().unwrap().iter().any(|a| a == "title:synthetictale"));
        assert_eq!(r["cutoff"]["policy"], "public_domain_historical");
    }
}

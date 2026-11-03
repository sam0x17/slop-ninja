//! FineWeb blog-register adapter: every row of the given parquet shards
//! whose URL is a blog page (`blogs::classify_url`), rendered into whole
//! first-person prose paragraphs.
//!
//! * **Selection.** By URL only (no row sample, no model or detector score):
//!   the blog rules of `blogs`, then FineWeb's metadata gates (capture before
//!   2022-11-30, `language == "en"`, `language_score >= min`, a registered
//!   domain not of another pool source).
//! * **Groups.** Unit and source family: one page (`fineweb_page:<uuid>`,
//!   the FineWeb pool's family key, so a page already in that pool is a
//!   prior family at dedup). Split group, bucket key and `writer_key`: the
//!   blog. Blogs on a non-hosted domain (Blogspot, custom domains) use
//!   FineWeb's `fineweb_site:<registered domain>`; hosted-platform blogs use
//!   `fineweb_blog:<blog id>`, except that a hosted blog with any page in
//!   the inheritance scan (the FineWeb pool's metadata scan, whose split
//!   group is the whole platform domain) takes that page's site key, so a
//!   blog never sits on both sides of the FineWeb split.
//! * **Paragraphs.** FineWeb's line screens (list-like lines, boilerplate,
//!   `Screen::MARKUP`), blog chrome lines (comment headers, labels, post
//!   navigation), and a blog template screen (a line of 8+ words on two or
//!   more rendered pages of one blog is a break). Then the shared paragraph
//!   run (first run reaching 220 words, 150..450), the English screen and
//!   the first-person screen (`blogs::FIRST_PERSON_RULE`).
use crate::{
    adapter::Run,
    blogs::{self, BlogUrl, FirstPerson, FirstPersonRule},
    fineweb::{self, COLUMNS, field_f64, field_i64, field_str, lines},
    psl::Psl,
    record::{self, Doc},
    text::{self, Para},
};
use anyhow::{Context, Result};
use parquet::{
    file::reader::{FileReader, SerializedFileReader},
    record::Field,
    schema::types::Type,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

pub const COLLECTION: &str = "fineweb_blog_cc_historical";
pub const DOMAIN: &str = "web_blog";
pub const METHOD: &str = "fineweb_text_lines_as_paragraphs;boilerplate_list_blog_chrome_and_blog_template_breaks;first_person_screen_v1";

pub struct Options<'a> {
    pub inputs: &'a [PathBuf],
    pub psl: &'a Psl,
    pub min_language_score: f64,
    pub excluded_domains: &'a HashSet<String>,
    pub dataset_revision: &'a str,
    /// Hosted blog key -> inherited site key (see the module documentation).
    pub inherit: &'a HashMap<String, String>,
    pub first_person: FirstPersonRule,
}

struct Page {
    id: String,
    uuid: String,
    url: String,
    dump: String,
    date: String,
    file_path: String,
    language_score: f64,
    token_count: i64,
    blog: BlogUrl,
    blog_key: String,
    group: String,
    domain: String,
    host: String,
    text: String,
    input: usize,
}

/// Hosted blog keys of a FineWeb metadata scan (`reserved-metadata.jsonl`)
/// mapped to the scan row's group key.
pub fn inheritance(psl: &Psl, scan_rows: impl Iterator<Item = Result<Value>>) -> Result<(HashMap<String, String>, u64)> {
    let mut m = HashMap::new();
    let mut rows = 0u64;
    for r in scan_rows {
        let r = r?;
        rows += 1;
        let url = r["url"].as_str().unwrap_or("");
        let Some(group) = r["group_key"].as_str() else { continue };
        if let Some((_, domain, b)) = blogs::classify_with(psl, url) {
            if b.hosted {
                m.insert(b.key(&domain), group.to_owned());
            }
        }
    }
    Ok((m, rows))
}

pub fn classify(line: &str, repeated_in_blog: bool) -> Para {
    let t = text::squash(line);
    if blogs::blog_chrome(&t) {
        return Para::non_prose(t, "blog_chrome_line");
    }
    fineweb::classify(line, repeated_in_blog)
}

pub fn adapt(opt: &Options, run: &mut Run) -> Result<Value> {
    let mut pages: Vec<Page> = Vec::new();
    let mut per_file = Vec::new();
    let mut by_rule_seen: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut inherited_blogs: HashSet<String> = HashSet::new();
    for (fi, path) in opt.inputs.iter().enumerate() {
        let (sha, bytes) = crate::io::file_sha(path)?;
        let reader = SerializedFileReader::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?)?;
        let fm = reader.metadata().file_metadata();
        let rows = fm.num_rows().max(0) as u64;
        let schema = fm.schema();
        let fields: Vec<Arc<Type>> = schema.get_fields().iter().filter(|f| COLUMNS.contains(&f.name())).cloned().collect();
        anyhow::ensure!(fields.len() == COLUMNS.len(), "{}: missing FineWeb columns", path.display());
        let proj = Type::group_type_builder(schema.name()).with_fields(fields).build()?;
        let (mut seen, mut blog_rows) = (0u64, 0u64);
        let mut file_rules: BTreeMap<&'static str, u64> = BTreeMap::new();
        for row in reader.get_row_iter(Some(proj))? {
            let row = row?;
            seen += 1;
            let mut v: HashMap<&str, &Field> = HashMap::new();
            for (k, f) in row.get_column_iter() {
                v.insert(k.as_str(), f);
            }
            let Some(id) = v.get("id").and_then(|f| field_str(f)) else {
                run.count("row_without_id");
                continue;
            };
            let url = v.get("url").and_then(|f| field_str(f)).unwrap_or_default();
            let Some((host, domain, blog)) = blogs::classify_with(opt.psl, &url) else {
                run.count("url_not_a_blog_page");
                continue;
            };
            blog_rows += 1;
            run.units_seen += 1;
            *file_rules.entry(blog.rule).or_default() += 1;
            *by_rule_seen.entry(blog.rule).or_default() += 1;
            let uuid = id.trim_start_matches("<urn:uuid:").trim_end_matches('>').to_owned();
            let unit = format!("fineweb_page:{uuid}");
            let blog_key = blog.key(&domain);
            let group = match opt.inherit.get(&blog_key) {
                Some(g) if blog.hosted => {
                    inherited_blogs.insert(blog_key.clone());
                    g.clone()
                }
                _ => blog_key.clone(),
            };
            let date = v.get("date").and_then(|f| field_str(f)).unwrap_or_default();
            let lang = v.get("language").and_then(|f| field_str(f)).unwrap_or_default();
            let score = v.get("language_score").and_then(|f| field_f64(f)).unwrap_or(0.0);
            let excluded = opt.excluded_domains.contains(&domain);
            let dump = v.get("dump").and_then(|f| field_str(f)).unwrap_or_default();
            let token_count = v.get("token_count").and_then(|f| field_i64(f)).unwrap_or(-1);
            let file_path = v.get("file_path").and_then(|f| field_str(f)).unwrap_or_default();
            let page_text = v.get("text").and_then(|f| field_str(f));
            let meta = || {
                json!({"fineweb_id": id, "url": url, "host": host, "registered_domain": domain, "blog_key": blog_key,
                       "blog_rule": blog.rule, "blog_platform": blog.platform, "group_inherited_from_scan": group != blog_key,
                       "dump": dump, "date": date, "language": lang, "language_score": score, "token_count": token_count,
                       "warc_file_path": file_path, "input_file": path.file_name().and_then(|n| n.to_str()), "input_sha256": sha,
                       "page_text_sha256": page_text.as_deref().map(|t| text::sha_hex(t.as_bytes())),
                       "excluded_registered_domain": excluded,
                       "eligible_metadata": record::before_cutoff(&date) && lang == "en" && score >= opt.min_language_score && !excluded && page_text.is_some()})
            };
            if !run.sampled_meta_grouped(&group, &unit, meta)? {
                continue;
            }
            let reason = if !record::before_cutoff(&date) {
                Some("crawl_date_not_before_2022_11_30")
            } else if lang != "en" {
                Some("fineweb_language_not_en")
            } else if score < opt.min_language_score {
                Some("fineweb_language_score_below_minimum")
            } else if excluded {
                Some("registered_domain_of_another_pool_source")
            } else if page_text.is_none() {
                Some("no_text")
            } else {
                None
            };
            if let Some(r) = reason {
                run.reject(&unit, r)?;
                continue;
            }
            pages.push(Page { id, uuid, url, dump, date, file_path, language_score: score, token_count, blog, blog_key, group, domain, host,
                              text: page_text.unwrap_or_default(), input: fi });
        }
        let d = json!({"role": "fineweb_parquet", "path": path, "sha256": sha, "bytes": bytes, "rows_in_file": rows, "rows_read": seen,
                       "blog_url_rows": blog_rows, "blog_url_rows_by_rule": file_rules,
                       "dataset": "HuggingFaceFW/fineweb", "dataset_revision": opt.dataset_revision});
        run.input(d.clone());
        per_file.push(d);
    }

    // Blog template screen over the pages rendered in this run.
    let mut blog_line: HashMap<(u64, u64), u32> = HashMap::new();
    for p in &pages {
        let bk = text::h64(p.blog_key.as_bytes());
        let mut mine: HashSet<u64> = HashSet::new();
        for l in lines(&p.text) {
            let n = text::normalized(l);
            if text::words(&n) >= 8 {
                mine.insert(text::h64(n.as_bytes()));
            }
        }
        for h in mine {
            *blog_line.entry((bk, h)).or_default() += 1;
        }
    }
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut accepted_by: BTreeMap<String, u64> = BTreeMap::new();
    let method = format!("{METHOD};{}", text::PARAGRAPH_RUN_METHOD);
    for p in &pages {
        if run.full() {
            break;
        }
        let unit = format!("fineweb_page:{}", p.uuid);
        let bk = text::h64(p.blog_key.as_bytes());
        let paras: Vec<Para> = lines(&p.text)
            .map(|l| {
                let n = text::normalized(l);
                let rep = text::words(&n) >= 8 && blog_line.get(&(bk, text::h64(n.as_bytes()))).copied().unwrap_or(0) >= 2;
                classify(l, rep)
            })
            .collect();
        for x in &paras {
            if let Some(r) = x.reason {
                *breaks.entry(r).or_default() += 1;
            }
        }
        let Some(ex) = text::paragraph_run(&paras) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            continue;
        };
        if let Some(r) = text::english_screen(&ex.text) {
            run.reject(&unit, r)?;
            continue;
        }
        let fp = FirstPerson::of(&ex.text);
        if !fp.passes(opt.first_person) {
            run.reject(&unit, "not_first_person_prose")?;
            continue;
        }
        let register = blogs::register(&ex.text);
        let rights = json!({
            "license": record::FINEWEB_LICENSE,
            "license_url": "https://opendatacommons.org/licenses/by/1-0/",
            "additional_terms_url": "https://commoncrawl.org/terms-of-use",
            "attribution": format!("FineWeb (HuggingFaceFW/fineweb, revision {}), ODC-By 1.0; Common Crawl {}; page {}", opt.dataset_revision, p.dump, p.url),
            "evidence_url": "https://huggingface.co/datasets/HuggingFaceFW/fineweb",
            "commercial_training": true,
            "model_release": true,
            "external_evaluation": true,
            "redistribute_text": false,
            "admission_decision": "user_2026-10-27: FineWeb admitted for Train, Train-mining and as an evaluation domain (crawl snapshots before 2022-11-30, text in ignored storage, never redistributed); user_2026-10-27: add blog-register human text through a FineWeb blog-platform subset grouped by blog/registered domain",
            "notes": "The database licence covers the FineWeb compilation; rights in each page's text remain with its owner and are not granted by the licence.",
        });
        let blog = json!({"blog_key": p.blog_key, "rule": p.blog.rule, "platform": p.blog.platform, "blog_id": p.blog.blog_id,
                          "hosted_platform": p.blog.hosted, "group_inherited_from_scan": p.group != p.blog_key});
        let doc = Doc {
            id: format!("pool-fwblog-{}", p.uuid),
            collection: COLLECTION,
            domain: DOMAIN,
            source_group: unit.clone(),
            writer_key: Some(p.group.clone()),
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: &method,
            unit: json!({"kind": "fineweb_page_text_lines", "span_basis": "nonempty_text_line_index_half_open", "fineweb_id": p.id,
                         "page_text_sha256": text::sha_hex(p.text.as_bytes()), "input_file": opt.inputs[p.input].file_name().and_then(|n| n.to_str()),
                         "first_person": fp.to_value(), "register_lexicon_label": register}),
            provenance: json!({"source_url": p.url, "host": p.host, "registered_domain": p.domain, "site_key": format!("fineweb_site:{}", p.domain),
                               "blog": blog, "fineweb_id": p.id, "dump": p.dump, "crawl_date": p.date, "warc_file_path": p.file_path,
                               "language_score": p.language_score, "token_count": p.token_count,
                               "dataset": "HuggingFaceFW/fineweb", "dataset_revision": opt.dataset_revision}),
            date: Some(p.date.clone()),
            date_basis: "common_crawl_capture_time_upper_bound_on_composition",
            cutoff_policy: "dated_before_cutoff",
            rights,
            aliases: vec![],
        };
        if run.accept(&unit, &doc)? {
            *accepted_by.entry(format!("rule:{}", p.blog.rule)).or_default() += 1;
            *accepted_by.entry(format!("platform:{}", p.blog.platform)).or_default() += 1;
            *accepted_by.entry(format!("register:{register}")).or_default() += 1;
            *accepted_by.entry(format!("voice:{}", fp.voice())).or_default() += 1;
            *accepted_by.entry(format!("dump:{}", p.dump)).or_default() += 1;
        }
    }
    Ok(json!({"files": per_file, "blog_url_rows_by_rule": by_rule_seen, "pages_rendered": pages.len(), "paragraph_breaks": breaks,
              "accepted_counts": accepted_by, "hosted_blogs_inheriting_scan_group": inherited_blogs.len(),
              "inheritance_keys": opt.inherit.len(),
              "blog_template_rule": "a line (8+ words) whose whitespace-normalised hash occurs on 2+ rendered pages of one blog is a break",
              "blog_chrome": blogs::BLOG_CHROME, "boilerplate_terms": fineweb::BOILERPLATE,
              "first_person_rule": {"min_singular": opt.first_person.min_singular, "min_plural": opt.first_person.min_plural, "min_rate": opt.first_person.min_rate,
                                    "singular_forms": blogs::FIRST_PERSON_SINGULAR, "plural_forms": blogs::FIRST_PERSON_PLURAL,
                                    "rule": "(singular >= min_singular or plural >= min_plural) and (singular + plural) / words >= min_rate on the excerpt"},
              "platforms": blogs::PLATFORMS.iter().map(|(d, l)| json!([d, l])).collect::<Vec<_>>(),
              "min_language_score": opt.min_language_score,
              "excluded_registered_domains": opt.excluded_domains.iter().collect::<std::collections::BTreeSet<_>>(),
              "psl_rules": opt.psl.rule_count,
              "register_lexicon": "counts-only descriptive tally (blogs::REGISTERS); never a selection or training feature",
              "grouping": "unit and source family: page; bucket, split group and writer_key: blog (fineweb_site:<registered domain> for non-hosted blogs, fineweb_blog:<blog id> for hosted-platform blogs unless inherited from the FineWeb scan)"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chrome_lines_break() {
        assert_eq!(classify("Anonymous said... what a lovely post, thank you so much for sharing it", false).reason, Some("blog_chrome_line"));
        assert!(classify("I spent the whole afternoon in the garden and my back still hurts tonight.", false).prose);
        assert_eq!(classify("I spent the whole afternoon in the garden and my back still hurts tonight.", true).reason, Some("repeated_on_site_template"));
    }
    #[test]
    fn inheritance_maps_hosted_blogs_only() {
        let psl = Psl::parse("com\nblogspot.com\n");
        let rows = vec![
            Ok(json!({"url": "https://cats.wordpress.com/2010/01/02/hello-world/", "group_key": "fineweb_site:wordpress.com"})),
            Ok(json!({"url": "https://anna.blogspot.com/2010/01/x.html", "group_key": "fineweb_site:anna.blogspot.com"})),
            Ok(json!({"url": "https://example.com/products", "group_key": "fineweb_site:example.com"})),
        ];
        let (m, n) = inheritance(&psl, rows.into_iter()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(m.len(), 1);
        assert_eq!(m["fineweb_blog:cats.wordpress.com"], "fineweb_site:wordpress.com");
    }
}

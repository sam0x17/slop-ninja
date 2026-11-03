//! Global Voices (English) adapter over WordPress REST post pages
//! (`/wp-json/wp/v2/posts`, JSON arrays of posts with rendered content).
//!
//! * **Rights.** Global Voices publishes under CC BY 3.0 by default
//!   (attribution policy); admitted by the user decision of 2026-10-27 for
//!   Train, Train-mining and as an evaluation domain.
//! * **Gates.** Published, unprotected `post` objects whose publication and
//!   last modification (GMT) are both before 2022-11-30. A post whose body
//!   carries an origin notice (translated, republished, partner or
//!   permission material) is rejected whole: its text may not be the
//!   posting author's own English writing.
//! * **Groups.** Unit and source family: one post (`global_voices_post:<id>`).
//!   Bucket, split group and `writer_key`: the WordPress posting account
//!   (`global_voices_author:<id>`). The public byline credit ("Written by",
//!   translator, coauthor) is not in the REST object; the posting account
//!   is the available contributor key, and organisational or shared
//!   accounts are not separated (recorded limits).
//! * **Paragraphs.** `html::render_with` (quotations, lists, headings,
//!   tables, images and captions break runs; `Screen::MARKUP`, so quoted
//!   spans of 8+ words also break), bracketed editorial notes break, then
//!   the shared paragraph run and the English screen.
use crate::{
    adapter::Run,
    html,
    record::{self, Doc},
    text::{self, Para, Screen},
    wikitext::decode_entities,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const COLLECTION: &str = "global_voices_historical";
pub const LICENSE: &str = "CC-BY-3.0";
pub const DROP_CLASSES: &[&str] = &[
    "wp-caption", "wp-caption-text", "gallery", "embed-youtube", "twitter-tweet", "instagram-media", "factbox", "translation",
    "inline-rss", "wp-block-embed", "wp-block-image", "wp-block-quote", "wp-block-pullquote", "sharedaddy", "jp-relatedposts",
];
/// Lowercase phrases of an origin notice; any one rejects the post.
pub const ORIGIN_NOTICES: &[&str] = &[
    "translated by", "translation by", "translated from", "originally published", "first published", "first appeared",
    "republish", "content partnership", "content-sharing", "partnership with", "reproduced with", "used with permission",
    "edited version", "cross-posted", "crossposted",
];

pub fn origin_notice(body_lower: &str) -> Option<&'static str> {
    ORIGIN_NOTICES.iter().copied().find(|n| body_lower.contains(n))
}

fn editorial_note(t: &str) -> bool {
    let s = t.trim();
    (s.starts_with('[') && s.ends_with(']')) || (s.starts_with('(') && s.ends_with(')'))
}

pub fn paragraphs(content_html: &str) -> Vec<Para> {
    html::render_with(content_html, Screen::MARKUP, DROP_CLASSES)
        .into_iter()
        .map(|p| if p.prose && editorial_note(&p.text) { Para::non_prose(p.text, "editorial_note") } else { p })
        .collect()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

pub fn adapt(pages: &[PathBuf], run: &mut Run) -> Result<Value> {
    let method = format!("wordpress_rest_rendered_content;slop_ninja_post_html_paragraphs_v1_with_class_drops;{}", text::PARAGRAPH_RUN_METHOD);
    let mut files = Vec::new();
    let mut posts_seen = 0u64;
    let mut breaks: std::collections::BTreeMap<&'static str, u64> = std::collections::BTreeMap::new();
    let mut prose_words_by_year: std::collections::BTreeMap<String, (u64, u64)> = std::collections::BTreeMap::new();
    for path in pages {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let sha = text::sha_hex(&bytes);
        let arr: Vec<Value> = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        files.push(json!({"file": path.file_name().and_then(|n| n.to_str()), "sha256": sha, "bytes": bytes.len(), "posts": arr.len()}));
        for p in &arr {
            if run.full() {
                break;
            }
            posts_seen += 1;
            run.units_seen += 1;
            let Some(pid) = p["id"].as_u64() else {
                run.count("post_without_id");
                continue;
            };
            let unit = format!("global_voices_post:{pid}");
            let Some(author) = p["author"].as_u64() else {
                run.reject(&unit, "no_author_account")?;
                continue;
            };
            let author_key = format!("global_voices_author:{author}");
            let content = s(&p["content"], "rendered");
            let title = decode_entities(s(&p["title"], "rendered"));
            let protected = p["content"]["protected"] == true;
            let (date, modified) = (s(p, "date_gmt").to_owned(), s(p, "modified_gmt").to_owned());
            let eligible = s(p, "status") == "publish" && s(p, "type") == "post" && !protected && record::before_cutoff(&date) && record::before_cutoff(&modified);
            let meta = || {
                json!({"post_id": pid, "author_account": author, "link": p["link"], "date_gmt": date, "modified_gmt": modified,
                       "status": p["status"], "type": p["type"], "content_protected": protected,
                       "content_html_sha256": text::sha_hex(content.as_bytes()), "title_sha256": text::sha_hex(title.as_bytes()),
                       "categories": p["categories"].as_array().map(Vec::len), "page_file": path.file_name().and_then(|n| n.to_str()),
                       "page_sha256": sha, "eligible_metadata": eligible})
            };
            if !run.sampled_meta_grouped(&author_key, &unit, meta)? {
                continue;
            }
            let reason = if s(p, "status") != "publish" || s(p, "type") != "post" {
                Some("not_published_post")
            } else if protected {
                Some("content_protected")
            } else if !record::before_cutoff(&date) {
                Some("published_not_before_2022_11_30")
            } else if !record::before_cutoff(&modified) {
                Some("modified_not_before_2022_11_30")
            } else {
                None
            };
            if let Some(r) = reason {
                run.reject(&unit, r)?;
                continue;
            }
            let paras = paragraphs(content);
            for x in &paras {
                if let Some(r) = x.reason {
                    *breaks.entry(r).or_default() += 1;
                }
            }
            let body_lower = paras.iter().map(|x| x.text.to_lowercase()).collect::<Vec<_>>().join("\n");
            if let Some(n) = origin_notice(&body_lower) {
                run.reject(&unit, &format!("origin_notice:{}", n.replace(' ', "_")))?;
                continue;
            }
            let year = date.get(..4).unwrap_or("").to_owned();
            prose_words_by_year.entry(year.clone()).or_default().0 += 1;
            let Some(ex) = text::paragraph_run(&paras) else {
                run.reject(&unit, "no_paragraph_run_within_bounds")?;
                continue;
            };
            if let Some(r) = text::english_screen(&ex.text) {
                run.reject(&unit, r)?;
                continue;
            }
            prose_words_by_year.entry(year).or_default().1 += 1;
            let link = s(p, "link").to_owned();
            let doc = Doc {
                id: format!("pool-gv-{pid}"),
                collection: COLLECTION,
                domain: "news_citizen_media",
                source_group: unit.clone(),
                writer_key: Some(author_key.clone()),
                evidence: "historical_proxy",
                excerpt: &ex,
                extraction_method: &method,
                unit: json!({"kind": "wordpress_post_content", "post_id": pid, "content_html_sha256": text::sha_hex(content.as_bytes()),
                             "page_file": path.file_name().and_then(|n| n.to_str()), "page_sha256": sha}),
                provenance: json!({"source_url": link, "post_id": pid, "author_account": author, "title": title, "date_gmt": date,
                                   "modified_gmt": modified, "categories": p["categories"], "slug": p["slug"],
                                   "api": "https://globalvoices.org/wp-json/wp/v2/posts", "site": "globalvoices.org (English)"}),
                date: Some(date.clone()),
                date_basis: "wordpress_date_gmt_publication",
                cutoff_policy: "dated_before_cutoff",
                rights: {
                    let mut r = record::rights(
                        LICENSE,
                        format!("{title}; Global Voices, {link}; posting account {author} (byline on the article page); CC BY 3.0; paragraphs extracted, markup omitted"),
                        "https://globalvoices.org/about/global-voices-attribution-policy/",
                        "Default Global Voices licence; item-level exceptions (partner or third-party material) are screened by origin notices only.",
                    );
                    r["admission_decision"] = json!("user_2026-10-27: Global Voices (CC BY 3.0) admitted for Train, Train-mining and as an evaluation domain; downloaded directly on build-host; articles before 2022-11-30; grouped by author account");
                    r["redistribute_text"] = json!(false);
                    r
                },
                aliases: vec![],
            };
            run.accept(&unit, &doc)?;
        }
    }
    let by_year: serde_json::Map<String, Value> = prose_words_by_year.into_iter().map(|(y, (n, k))| (y, json!({"screened_posts": n, "with_excerpt": k}))).collect();
    Ok(json!({"files": files, "posts_seen": posts_seen, "paragraph_breaks": breaks, "excerpt_yield_by_publication_year": by_year, "drop_classes": DROP_CLASSES, "origin_notices": ORIGIN_NOTICES,
              "grouping": "unit and source family: post; bucket, split group and writer_key: WordPress posting account",
              "limits": ["posting account is not the public byline credit; translator and coauthor roles are not visible in the REST object",
                         "organisational or shared accounts are not separated", "origin-notice screen is phrase based"]}))
}

/// Page files of a download directory in name order (receipts excluded).
pub fn page_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("page-")).and_then(|n| n.strip_suffix(".json")).is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    v.sort();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notes_captions_and_origin_notices() {
        let html = "<p>[All links lead to Spanish-language pages unless otherwise noted.]</p><div class=\"wp-caption\"><p class=\"wp-caption-text\">Photo of a street market taken in the city centre last year.</p></div><p>The writer's own paragraph about the market comes after the caption.</p>";
        let p = paragraphs(html);
        assert_eq!(p[0].reason, Some("editorial_note"));
        assert_eq!(p[1].reason, Some("dropped_class_element"));
        assert!(p[2].prose);
        assert_eq!(origin_notice("this post was originally published on another site."), Some("originally published"));
        assert_eq!(origin_notice("an ordinary article body"), None);
        let d = tempfile::tempdir().unwrap();
        for n in ["page-0002.json", "page-0001.json", "page-0001.json.receipt.json", "page-0003.json.part", "categories-01.json"] {
            std::fs::write(d.path().join(n), "[]").unwrap();
        }
        let names: Vec<String> = page_files(d.path()).unwrap().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["page-0001.json", "page-0002.json"]);
    }
}

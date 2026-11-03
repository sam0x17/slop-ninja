//! PLOS Blogs adapter over WordPress REST post pages, one directory per
//! blog subdomain (`<posts-root>/<blog>/page-N.json`).
//!
//! * **Rights.** plos.org/blogs/about: "Unless otherwise noted, material
//!   posted to any PLOS staff or independent blog on the PLOS Blog Network"
//!   is under CC BY 4.0, while "Certain Independent blogs housed under the
//!   heading of 'Archived Blogs' may maintain a CC BY-NC" licence. Archived
//!   blogs are therefore excluded whole (their licence cannot be confirmed
//!   per archive), and every current-blog post gets a per-post licence
//!   check: a non-commercial, no-derivatives or all-rights-reserved notice
//!   or Creative Commons link in the post, or an origin notice (republished,
//!   cross-posted, translated or permission material), rejects the post.
//! * **Gates.** Published, unprotected `post` objects published (GMT)
//!   before 2022-11-30. Every post's `modified_gmt` lies after the cutoff (2023-2026; almost all 2024)
//!   (a platform migration), so modification dates cannot gate; they are
//!   recorded and the limit is reported.
//! * **Groups.** Unit and source family: one post
//!   (`plos_blog_post:<blog>/<id>`). Split group and `writer_key`: the
//!   posting writer by folded display name (`plos_blog_writer:<name key>`),
//!   or the blog and account id when no name is known. Train-side only:
//!   the PLOS evaluation reservations are fixed (user decision 2026-10-27).
//! * **Paragraphs.** As Global Voices (`global_voices::paragraphs`), then
//!   the shared paragraph run and the English screen. No first-person
//!   screen; first-person counts and the register label are recorded.
use crate::{
    adapter::Run,
    blogs::{self, FirstPerson},
    global_voices,
    record::{self, Doc},
    text,
    wikitext::decode_entities,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

pub const COLLECTION: &str = "plos_blog_historical";
pub const DOMAIN: &str = "science_blog";
pub const LICENSE: &str = "CC-BY-4.0";
pub const EVIDENCE_URL: &str = "https://plos.org/blogs/about/";

/// Lowercase licence notices that mean the post is not plainly CC BY.
pub const LICENCE_NOTICES: &[&str] = &[
    "by-nc", "by nc", "non-commercial", "noncommercial", "by-nd", "no derivatives", "noderivatives", "all rights reserved",
    "\u{a9}", "(c) copyright", "reprinted with permission", "reprinted from", "courtesy of the publisher",
];

pub fn licence_notice(lower: &str) -> Option<&'static str> {
    LICENCE_NOTICES.iter().copied().find(|n| lower.contains(n))
}

/// Creative Commons licence links in the post HTML that are not CC BY or CC0.
pub fn restrictive_cc_link(html_lower: &str) -> Option<String> {
    let mut rest = html_lower;
    while let Some(i) = rest.find("creativecommons.org/licenses/") {
        let tail = &rest[i + "creativecommons.org/licenses/".len()..];
        let kind: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        if kind != "by" && kind != "by-sa" {
            return Some(kind);
        }
        rest = tail;
    }
    None
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn page_files(dir: &Path) -> Result<Vec<PathBuf>> {
    global_voices::page_files(dir)
}

/// Writer names of a blog: WordPress users (`users-*.json`) and Co-Authors
/// Plus terms (`coauthors-*.json`), id -> (name, slug).
fn names(dir: &Path, prefix: &str) -> Result<HashMap<u64, (String, String)>> {
    let mut m = HashMap::new();
    if !dir.is_dir() {
        return Ok(m);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".json") && !n.contains("receipt")))
        .collect();
    files.sort();
    for f in files {
        let arr: Vec<Value> = serde_json::from_slice(&std::fs::read(&f)?).with_context(|| format!("parse {}", f.display()))?;
        for u in arr {
            if let Some(id) = u["id"].as_u64() {
                let n = decode_entities(s(&u, "name")).trim().to_owned();
                if !n.is_empty() {
                    m.insert(id, (n, s(&u, "slug").to_owned()));
                }
            }
        }
    }
    Ok(m)
}

pub struct Options<'a> {
    pub posts_root: &'a Path,
    pub authors_root: Option<&'a Path>,
    pub blogs: &'a [String],
    pub archived: &'a [String],
}

pub fn adapt(opt: &Options, run: &mut Run) -> Result<Value> {
    let method = format!("wordpress_rest_rendered_content;slop_ninja_post_html_paragraphs_v1_with_class_drops;{}", text::PARAGRAPH_RUN_METHOD);
    let mut files = Vec::new();
    let mut per_blog: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut modified_years: BTreeMap<String, u64> = BTreeMap::new();
    for blog in opt.archived {
        anyhow::ensure!(!opt.blogs.contains(blog), "blog {blog} is both current and archived");
        let dir = opt.posts_root.join(blog);
        if dir.is_dir() {
            for path in page_files(&dir)? {
                let arr: Vec<Value> = serde_json::from_slice(&std::fs::read(&path)?)?;
                for p in &arr {
                    run.units_seen += 1;
                    run.reject(&format!("plos_blog_post:{blog}/{}", p["id"]), "archived_blog_licence_may_be_cc_by_nc")?;
                }
            }
        }
    }
    for blog in opt.blogs {
        let dir = opt.posts_root.join(blog);
        let pages = page_files(&dir)?;
        anyhow::ensure!(!pages.is_empty(), "no page-*.json files in {}", dir.display());
        let (users, coauthors) = match opt.authors_root {
            Some(a) => (names(&a.join(blog), "users-")?, names(&a.join(blog), "coauthors-")?),
            None => (HashMap::new(), HashMap::new()),
        };
        let tally = per_blog.entry(blog.clone()).or_default();
        for path in pages {
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let sha = text::sha_hex(&bytes);
            let arr: Vec<Value> = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
            files.push(json!({"blog": blog, "file": path.file_name().and_then(|n| n.to_str()), "sha256": sha, "bytes": bytes.len(), "posts": arr.len()}));
            for p in &arr {
                if run.full() {
                    break;
                }
                run.units_seen += 1;
                *tally.entry("posts".into()).or_default() += 1;
                let Some(pid) = p["id"].as_u64() else {
                    run.count("post_without_id");
                    continue;
                };
                let unit = format!("plos_blog_post:{blog}/{pid}");
                let author = p["author"].as_u64();
                let co: Vec<u64> = p["coauthors"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect();
                // Display names: each Co-Authors Plus byline term first, then the posting account, resolved to the
                // user whose slug equals the term name when there is one (terms are named by username).
                let mut writer_names: Vec<String> = Vec::new();
                let resolve = |c: &u64| {
                    coauthors.get(c).map(|(n, _)| users.values().find(|(_, slug)| slug == n).map(|(d, _)| d.clone()).unwrap_or_else(|| n.clone()))
                };
                for n in co.iter().filter_map(resolve).chain(author.and_then(|a| users.get(&a)).map(|(n, _)| n.clone())) {
                    if !writer_names.contains(&n) {
                        writer_names.push(n);
                    }
                }
                let writer_key = match (writer_names.first(), author) {
                    (Some(n), _) if !text::underscore_key(n).is_empty() => format!("plos_blog_writer:{}", text::underscore_key(n)),
                    (_, Some(a)) => format!("plos_blog_account:{blog}/{a}"),
                    _ => {
                        run.reject(&unit, "no_author_account")?;
                        continue;
                    }
                };
                let content = s(&p["content"], "rendered");
                let title = decode_entities(s(&p["title"], "rendered"));
                let protected = p["content"]["protected"] == true;
                let (date, modified) = (s(p, "date_gmt").to_owned(), s(p, "modified_gmt").to_owned());
                *modified_years.entry(modified.get(..4).unwrap_or("").to_owned()).or_default() += 1;
                let meta = || {
                    json!({"post_id": pid, "blog": blog, "author_account": author, "coauthor_terms": co, "link": p["link"], "date_gmt": date,
                           "modified_gmt": modified, "content_html_sha256": text::sha_hex(content.as_bytes()), "page_sha256": sha})
                };
                if !run.sampled_meta_grouped(&writer_key, &unit, meta)? {
                    continue;
                }
                let lower_html = content.to_lowercase();
                let reason = if s(p, "status") != "publish" || s(p, "type") != "post" {
                    Some("not_published_post".to_owned())
                } else if protected {
                    Some("content_protected".to_owned())
                } else if !record::before_cutoff(&date) {
                    Some("published_not_before_2022_11_30".to_owned())
                } else if let Some(k) = restrictive_cc_link(&lower_html) {
                    Some(format!("restrictive_cc_link:{k}"))
                } else {
                    None
                };
                if let Some(r) = reason {
                    *tally.entry(r.clone()).or_default() += 1;
                    run.reject(&unit, &r)?;
                    continue;
                }
                let paras = global_voices::paragraphs(content);
                let body_lower = paras.iter().map(|x| x.text.to_lowercase()).collect::<Vec<_>>().join("\n");
                let notice = licence_notice(&body_lower).map(|n| format!("licence_notice:{}", n.replace(' ', "_")))
                    .or_else(|| global_voices::origin_notice(&body_lower).map(|n| format!("origin_notice:{}", n.replace(' ', "_"))));
                if let Some(r) = notice {
                    *tally.entry(r.clone()).or_default() += 1;
                    run.reject(&unit, &r)?;
                    continue;
                }
                let Some(ex) = text::paragraph_run(&paras) else {
                    *tally.entry("no_paragraph_run_within_bounds".into()).or_default() += 1;
                    run.reject(&unit, "no_paragraph_run_within_bounds")?;
                    continue;
                };
                if let Some(r) = text::english_screen(&ex.text) {
                    run.reject(&unit, r)?;
                    continue;
                }
                let fp = FirstPerson::of(&ex.text);
                let register = blogs::register(&ex.text);
                let link = s(p, "link").to_owned();
                let byline = if writer_names.is_empty() { format!("account {}", author.unwrap_or(0)) } else { writer_names.join(", ") };
                let mut rights = record::rights(
                    LICENSE,
                    format!("{title}; {byline}; PLOS Blogs ({blog}.plos.org), {link}; CC BY 4.0; paragraphs extracted, markup omitted"),
                    EVIDENCE_URL,
                    "PLOS Blogs Network default licence (\"Unless otherwise noted\"); per-post check rejects NC/ND/all-rights-reserved notices, restrictive CC links and origin notices; archived blogs (possible CC BY-NC) excluded whole.",
                );
                rights["admission_decision"] = json!("user_2026-10-27: add blog-register human text through CC BY PLOS blogs with a per-post licence check (exclude any CC BY-NC archive); Train and Train-mining only");
                rights["redistribute_text"] = json!(false);
                rights["licence_basis"] = json!("network_default_cc_by_4_0_with_per_post_check");
                let doc = Doc {
                    id: format!("pool-plosblog-{blog}-{pid}"),
                    collection: COLLECTION,
                    domain: DOMAIN,
                    source_group: unit.clone(),
                    writer_key: Some(writer_key.clone()),
                    evidence: "historical_proxy",
                    excerpt: &ex,
                    extraction_method: &method,
                    unit: json!({"kind": "wordpress_post_content", "blog": blog, "post_id": pid, "content_html_sha256": text::sha_hex(content.as_bytes()),
                                 "page_file": path.file_name().and_then(|n| n.to_str()), "page_sha256": sha,
                                 "first_person": fp.to_value(), "register_lexicon_label": register}),
                    provenance: json!({"source_url": link, "blog": blog, "site": format!("{blog}.plos.org"), "post_id": pid, "author_account": author,
                                       "coauthor_terms": co, "authors": writer_names, "title": title, "date_gmt": date, "modified_gmt": modified,
                                       "modified_note": "every post's modified_gmt lies after the cutoff (almost all in 2024, a platform migration); edits after the cutoff cannot be ruled out",
                                       "categories": p["categories"], "slug": p["slug"], "api": format!("https://{blog}.plos.org/wp-json/wp/v2/posts")}),
                    date: Some(date.clone()),
                    date_basis: "wordpress_date_gmt_publication",
                    cutoff_policy: "dated_before_cutoff",
                    rights,
                    aliases: vec![],
                };
                if run.accept(&unit, &doc)? {
                    *tally.entry("accepted".into()).or_default() += 1;
                }
            }
        }
    }
    Ok(json!({"files": files, "per_blog": per_blog, "modified_gmt_years": modified_years, "current_blogs": opt.blogs, "archived_blogs_excluded": opt.archived,
              "licence_notices": LICENCE_NOTICES, "origin_notices": global_voices::ORIGIN_NOTICES, "drop_classes": global_voices::DROP_CLASSES,
              "grouping": "unit and source family: post; split group and writer_key: writer display name (users and Co-Authors Plus terms), else blog account",
              "limits": ["every post's modified_gmt lies after the cutoff (almost all 2024, a platform migration), so post-cutoff edits cannot be excluded",
                         "licence and origin screens are phrase and link based",
                         "archived blogs are excluded whole although some may be CC BY",
                         "writer names come from WordPress users and Co-Authors Plus terms; organisational accounts are not separated"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn licence_screens() {
        assert_eq!(restrictive_cc_link("<a href=\"https://creativecommons.org/licenses/by/4.0/\">x</a>"), None);
        assert_eq!(restrictive_cc_link("<a href=\"https://creativecommons.org/licenses/by/4.0/\">x</a> <a href=\"http://creativecommons.org/licenses/by-nc-nd/4.0/\">"), Some("by-nc-nd".into()));
        assert_eq!(licence_notice("the cartoons are my own (cc by-nc-nd license)"), Some("by-nc"));
        assert_eq!(licence_notice("photo \u{a9} someone"), Some("\u{a9}"));
        assert_eq!(licence_notice("we discuss open access and copyright reform at length"), None);
    }
}

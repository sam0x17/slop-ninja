use crate::{SCHEMA, TOKENIZER, file_hash, hash, new_directory, write_json};
use anyhow::{Context, Result, ensure};
use grammar_core::features::words;
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Options {
    pub authors: usize,
    pub min_posts: usize,
    pub min_post_words: u64,
    pub max_post_words: u64,
    pub max_posts_per_author: usize,
    pub min_total_words: u64,
    pub seed: String,
    pub allow_placeholders: bool,
    /// Prior summaries whose union of authors must be absent from this export.
    /// One path keeps the original JSON string form; multiple paths use an array.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_paths",
        deserialize_with = "deserialize_paths"
    )]
    pub exclude_authors_from: Vec<PathBuf>,
}

fn serialize_paths<S: serde::Serializer>(
    paths: &[PathBuf],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if let [path] = paths {
        path.serialize(serializer)
    } else {
        paths.serialize(serializer)
    }
}

fn deserialize_paths<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<PathBuf>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Paths {
        One(PathBuf),
        Many(Vec<PathBuf>),
        Empty(()),
    }
    Ok(match Paths::deserialize(deserializer)? {
        Paths::One(path) => vec![path],
        Paths::Many(paths) => paths,
        Paths::Empty(()) => Vec::new(),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorSummary {
    pub author_id: String,
    pub manifest: String,
    pub selected_posts: usize,
    pub selected_words: u64,
    pub full_pool_posts: usize,
    pub full_pool_words: u64,
    pub split_posts: BTreeMap<String, usize>,
    pub split_words: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Summary {
    pub schema: String,
    pub archive_sha256: String,
    pub database_sha256: String,
    pub options: Options,
    pub full_pool_eligible_authors: usize,
    pub capped_selection_rejected_for_words: usize,
    pub capped_selection_rejected_for_dates: usize,
    pub eligible_authors_after_caps: usize,
    pub selected_authors: usize,
    pub selected_posts: usize,
    pub selected_words: u64,
    pub selection_policy: String,
    pub split_policy: String,
    pub authors: Vec<AuthorSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusions: Option<ExclusionSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExclusionSummary {
    /// Legacy single-summary fields remain present for exactly one input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_summary: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_summary_sha256: Option<String>,
    /// Multiple inputs bind each summary independently; author_ids below is their union.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_summaries: Vec<ExclusionSource>,
    /// Canonical SHA-256 of the compact JSON array of lexically sorted IDs.
    pub author_ids_sha256: String,
    pub author_ids: Vec<String>,
    pub eligible_authors_removed: usize,
    pub eligible_authors_remaining: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExclusionSource {
    pub source_summary: PathBuf,
    pub source_summary_sha256: String,
    pub author_ids_sha256: String,
    pub author_ids: Vec<String>,
}

fn load_exclusions(
    path: &Path,
    db: &Connection,
    archive_sha256: &str,
    database_sha256: &str,
) -> Result<ExclusionSummary> {
    let bytes = fs::read(path).with_context(|| format!("read exclusions {}", path.display()))?;
    let prior: Summary = serde_json::from_slice(&bytes).context("parse prior export summary")?;
    ensure!(
        prior.schema == "unslop-blog-author-export-v1",
        "unsupported exclusion summary schema"
    );
    ensure!(
        prior.archive_sha256 == archive_sha256,
        "exclusion summary archive hash mismatch"
    );
    ensure!(
        prior.database_sha256 == database_sha256,
        "exclusion summary database hash mismatch"
    );
    ensure!(
        prior.selected_authors > 0 && prior.selected_authors == prior.authors.len(),
        "exclusion summary author count mismatch"
    );
    ensure!(
        prior.selected_posts
            == prior
                .authors
                .iter()
                .map(|author| author.selected_posts)
                .sum::<usize>(),
        "exclusion summary post count mismatch"
    );
    ensure!(
        prior.selected_words
            == prior
                .authors
                .iter()
                .map(|author| author.selected_words)
                .sum::<u64>(),
        "exclusion summary word count mismatch"
    );
    let mut ids = BTreeSet::new();
    for author in prior.authors {
        ensure!(
            !author.author_id.is_empty()
                && author.author_id.bytes().all(|byte| byte.is_ascii_digit()),
            "unsafe excluded author ID"
        );
        ensure!(
            ids.insert(author.author_id.clone()),
            "duplicate excluded author ID: {}",
            author.author_id
        );
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM authors WHERE author_id=?1)",
            [&author.author_id],
            |row| row.get(0),
        )?;
        ensure!(
            exists,
            "excluded author ID missing from corpus: {}",
            author.author_id
        );
        ensure!(
            author.selected_posts > 0
                && author.selected_posts == author.split_posts.values().sum::<usize>(),
            "excluded author post count mismatch: {}",
            author.author_id
        );
        ensure!(
            author.selected_words > 0
                && author.selected_words == author.split_words.values().sum::<u64>(),
            "excluded author word count mismatch: {}",
            author.author_id
        );
    }
    let author_ids: Vec<_> = ids.into_iter().collect();
    Ok(ExclusionSummary {
        source_summary: Some(path.to_path_buf()),
        source_summary_sha256: Some(hash(&bytes)),
        source_summaries: Vec::new(),
        author_ids_sha256: hash(&serde_json::to_vec(&author_ids)?),
        author_ids,
        eligible_authors_removed: 0,
        eligible_authors_remaining: 0,
    })
}

fn load_exclusion_union(
    paths: &[PathBuf],
    db: &Connection,
    archive_sha256: &str,
    database_sha256: &str,
) -> Result<Option<ExclusionSummary>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let mut summaries = paths
        .iter()
        .map(|path| load_exclusions(path, db, archive_sha256, database_sha256))
        .collect::<Result<Vec<_>>>()?;
    if summaries.len() == 1 {
        return Ok(summaries.pop());
    }
    let author_ids: Vec<_> = summaries
        .iter()
        .flat_map(|summary| summary.author_ids.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let source_summaries = summaries
        .into_iter()
        .map(|summary| ExclusionSource {
            source_summary: summary
                .source_summary
                .expect("single input has source path"),
            source_summary_sha256: summary
                .source_summary_sha256
                .expect("single input has source hash"),
            author_ids_sha256: summary.author_ids_sha256,
            author_ids: summary.author_ids,
        })
        .collect();
    Ok(Some(ExclusionSummary {
        source_summary: None,
        source_summary_sha256: None,
        source_summaries,
        author_ids_sha256: hash(&serde_json::to_vec(&author_ids)?),
        author_ids,
        eligible_authors_removed: 0,
        eligible_authors_remaining: 0,
    }))
}

#[derive(Clone, Debug)]
struct Post {
    id: i64,
    source_key: String,
    date: String,
    words: u64,
    sha256: String,
}

struct Author {
    id: String,
    posts: Vec<Post>,
    full_pool_posts: usize,
    full_pool_words: u64,
}

struct StoredPost {
    text: String,
    entry_name: String,
    entry_sha256: String,
    encoding: String,
    raw_start: u64,
    raw_end: u64,
    body_start: u64,
    body_end: u64,
    trimmed_start: u64,
    trimmed_end: u64,
    raw_sha256: String,
    raw_body_sha256: String,
    normalized_sha256: String,
    has_placeholder: bool,
}

fn rank(domain: &str, seed: &str, id: &str) -> String {
    hash(
        serde_json::to_string(&[domain, seed, id])
            .expect("string array serializes")
            .as_bytes(),
    )
}

/// Whole calendar dates stay together, in chronological order. The rounding
/// guarantees nonempty splits and approximately 80/10/10 percent of date groups.
fn split_dates(posts: &[Post]) -> Result<BTreeMap<String, &'static str>> {
    let mut dates: Vec<_> = posts.iter().map(|post| post.date.clone()).collect();
    dates.sort();
    dates.dedup();
    ensure!(
        dates.len() >= 3,
        "three distinct dates required for independent splits"
    );
    let train_end = (dates.len() * 8 / 10).clamp(1, dates.len() - 2);
    let dev_end = (dates.len() * 9 / 10).clamp(train_end + 1, dates.len() - 1);
    Ok(dates
        .into_iter()
        .enumerate()
        .map(|(index, date)| {
            (
                date,
                if index < train_end {
                    "train"
                } else if index < dev_end {
                    "dev"
                } else {
                    "test"
                },
            )
        })
        .collect())
}

pub fn export_authors(database: &Path, out: &Path, options: &Options) -> Result<Summary> {
    ensure!(options.authors > 0, "authors must be positive");
    ensure!(
        options.min_posts >= 3,
        "at least three posts required for train/dev/test"
    );
    ensure!(
        options.min_post_words > 0,
        "minimum post words must be positive"
    );
    ensure!(
        options.max_post_words >= options.min_post_words,
        "invalid post word range"
    );
    ensure!(
        options.max_posts_per_author >= options.min_posts,
        "post cap is below minimum posts"
    );
    let db = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let metadata = |key| -> Result<String> {
        Ok(
            db.query_row("SELECT value FROM metadata WHERE key=?1", [key], |row| {
                row.get(0)
            })?,
        )
    };
    ensure!(metadata("schema")? == SCHEMA, "unsupported corpus schema");
    ensure!(metadata("complete")? == "true", "incomplete corpus import");
    ensure!(metadata("tokenizer")? == TOKENIZER, "tokenizer mismatch");
    let archive_sha256 = metadata("archive_sha256")?;
    let database_sha256 = file_hash(database)?;
    let mut exclusions = load_exclusion_union(
        &options.exclude_authors_from,
        &db,
        &archive_sha256,
        &database_sha256,
    )?;
    let mut pool = db.prepare(
        "SELECT author_id,COUNT(*),SUM(word_count) FROM posts
         WHERE status='retained' AND word_count BETWEEN ?1 AND ?2
           AND has_c1_controls=0 AND (has_placeholder=0 OR ?3)
         GROUP BY author_id HAVING COUNT(*)>=?4 AND SUM(word_count)>=?5
         ORDER BY author_id",
    )?;
    let rows = pool.query_map(
        params![
            options.min_post_words as i64,
            options.max_post_words as i64,
            options.allow_placeholders,
            options.min_posts as i64,
            options.min_total_words as i64
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, usize>(1)?,
                row.get::<_, u64>(2)?,
            ))
        },
    )?;
    let mut full_pool_eligible_authors = 0;
    let mut capped_selection_rejected_for_words = 0;
    let mut capped_selection_rejected_for_dates = 0;
    let mut eligible = Vec::new();
    for row in rows {
        let (author, full_pool_posts, full_pool_words) = row?;
        full_pool_eligible_authors += 1;
        ensure!(
            !author.is_empty() && author.bytes().all(|byte| byte.is_ascii_digit()),
            "unsafe author ID in database"
        );
        let mut statement = db.prepare_cached(
            "SELECT id,source_key,authored_date,word_count,exact_text_sha256 FROM posts
             WHERE author_id=?1 AND status='retained' AND word_count BETWEEN ?2 AND ?3
               AND has_c1_controls=0 AND (has_placeholder=0 OR ?4) ORDER BY source_key",
        )?;
        let mut posts = statement
            .query_map(
                params![
                    author,
                    options.min_post_words as i64,
                    options.max_post_words as i64,
                    options.allow_placeholders
                ],
                |row| {
                    Ok(Post {
                        id: row.get(0)?,
                        source_key: row.get(1)?,
                        date: row.get(2)?,
                        words: row.get(3)?,
                        sha256: row.get(4)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        posts.sort_by_cached_key(|post| {
            (
                rank("post", &options.seed, &post.source_key),
                post.source_key.clone(),
            )
        });
        posts.truncate(options.max_posts_per_author);
        if posts.iter().map(|post| post.words).sum::<u64>() < options.min_total_words {
            capped_selection_rejected_for_words += 1;
            continue;
        }
        posts.sort_by(|left, right| {
            (&left.date, &left.source_key).cmp(&(&right.date, &right.source_key))
        });
        if split_dates(&posts).is_err() {
            capped_selection_rejected_for_dates += 1;
            continue;
        }
        eligible.push(Author {
            id: author,
            posts,
            full_pool_posts,
            full_pool_words,
        });
    }
    let eligible_authors_after_caps = eligible.len();
    if let Some(exclusions) = &mut exclusions {
        let ids: BTreeSet<_> = exclusions.author_ids.iter().collect();
        eligible.retain(|author| !ids.contains(&author.id));
        exclusions.eligible_authors_removed = eligible_authors_after_caps - eligible.len();
        exclusions.eligible_authors_remaining = eligible.len();
        ensure!(
            eligible.len() >= options.authors,
            "only {} eligible authors remain after exclusions; {} requested",
            eligible.len(),
            options.authors
        );
    }
    ensure!(
        !eligible.is_empty(),
        "no authors meet the selected post, word, and date requirements"
    );
    eligible.sort_by_cached_key(|author| {
        (rank("author", &options.seed, &author.id), author.id.clone())
    });
    eligible.truncate(options.authors);
    new_directory(out)?;
    let mut authors = Vec::new();
    for author in eligible {
        let author_dir = out.join("authors").join(&author.id);
        fs::create_dir_all(author_dir.join("texts"))?;
        let mut manifest = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(author_dir.join("references.jsonl"))?;
        let dates = split_dates(&author.posts)?;
        let mut split_posts = BTreeMap::new();
        let mut split_words = BTreeMap::new();
        for post in &author.posts {
            let split = dates[&post.date];
            let StoredPost {
                text,
                entry_name,
                entry_sha256,
                encoding,
                raw_start,
                raw_end,
                body_start,
                body_end,
                trimmed_start,
                trimmed_end,
                raw_sha256,
                raw_body_sha256,
                normalized_sha256,
                has_placeholder,
            } = db.query_row(
                "SELECT p.exact_text,e.name,e.raw_sha256,e.encoding,p.raw_start,p.raw_end,
                 p.body_start,p.body_end,p.trimmed_start,p.trimmed_end,p.raw_sha256,
                 p.raw_body_sha256,p.normalized_sha256,p.has_placeholder
                 FROM posts p JOIN entries e ON e.id=p.entry_id WHERE p.id=?1",
                [post.id],
                |row| {
                    Ok(StoredPost {
                        text: row.get(0)?,
                        entry_name: row.get(1)?,
                        entry_sha256: row.get(2)?,
                        encoding: row.get(3)?,
                        raw_start: row.get(4)?,
                        raw_end: row.get(5)?,
                        body_start: row.get(6)?,
                        body_end: row.get(7)?,
                        trimmed_start: row.get(8)?,
                        trimmed_end: row.get(9)?,
                        raw_sha256: row.get(10)?,
                        raw_body_sha256: row.get(11)?,
                        normalized_sha256: row.get(12)?,
                        has_placeholder: row.get(13)?,
                    })
                },
            )?;
            ensure!(
                hash(text.as_bytes()) == post.sha256,
                "stored text hash mismatch for {}",
                post.source_key
            );
            ensure!(
                words(&text).len() as u64 == post.words,
                "stored word count mismatch for {}",
                post.source_key
            );
            let filename = format!("texts/{}.txt", post.id);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(author_dir.join(&filename))?
                .write_all(text.as_bytes())?;
            let row = json!({
                "id": format!("blog:{}:{}", author.id, post.source_key),
                "path": filename,
                "source_group": format!("blog:{}:date:{}", author.id, post.date),
                "split": split,
                "authorship": "human",
                "author_id": author.id,
                "text_sha256": post.sha256,
                "provenance": {
                    "label": "corpus_attributed_human",
                    "dataset": "Blog Authorship Corpus 2004",
                    "archive_sha256": archive_sha256,
                    "zip_entry": entry_name,
                    "zip_entry_sha256": entry_sha256,
                    "source_post": post.source_key,
                    "supplied_composition_date": post.date,
                    "date_confidence": "dataset metadata; not independently verified",
                    "encoding": encoding,
                    "raw_post_byte_span": [raw_start,raw_end],
                    "raw_body_byte_span": [body_start,body_end],
                    "trimmed_body_byte_span": [trimmed_start,trimmed_end],
                    "raw_post_sha256": raw_sha256,
                    "raw_body_sha256": raw_body_sha256,
                    "text_sha256": post.sha256,
                    "excerpt_sha256": post.sha256,
                    "normalized_duplicate_sha256": normalized_sha256,
                    "word_count": post.words,
                    "tokenizer": TOKENIZER,
                    "has_placeholder": has_placeholder,
                    "has_c1_controls": false,
                    "quotation_and_copying": "unassessed beyond exact/whitespace duplicate groups",
                    "language": "unassessed per post",
                    "license_scope": "local noncommercial research; do not redistribute corpus text"
                }
            });
            serde_json::to_writer(&mut manifest, &row)?;
            manifest.write_all(b"\n")?;
            *split_posts.entry(split.to_owned()).or_default() += 1;
            *split_words.entry(split.to_owned()).or_default() += post.words;
        }
        let summary = AuthorSummary {
            author_id: author.id.clone(),
            manifest: format!("authors/{}/references.jsonl", author.id),
            selected_posts: author.posts.len(),
            selected_words: author.posts.iter().map(|post| post.words).sum(),
            full_pool_posts: author.full_pool_posts,
            full_pool_words: author.full_pool_words,
            split_posts,
            split_words,
        };
        write_json(&author_dir.join("summary.json"), &summary)?;
        authors.push(summary);
    }
    let summary = Summary {
        schema: "unslop-blog-author-export-v1".to_owned(),
        archive_sha256,
        database_sha256,
        options: options.clone(),
        full_pool_eligible_authors,
        capped_selection_rejected_for_words,
        capped_selection_rejected_for_dates,
        eligible_authors_after_caps,
        selected_authors: authors.len(),
        selected_posts: authors.iter().map(|author| author.selected_posts).sum(),
        selected_words: authors.iter().map(|author| author.selected_words).sum(),
        selection_policy: "seeded SHA-256 rank over author IDs and source post IDs; cap posts before eligibility; whole-post inputs, no concatenation".to_owned(),
        split_policy: "chronological calendar-date groups, approximately 80/10/10 train/dev/test with all splits nonempty; no date shared between splits".to_owned(),
        authors,
        exclusions,
    };
    write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_post_on_one_date_shares_a_split() {
        let dates = [
            "2004-01-01",
            "2004-01-01",
            "2004-01-02",
            "2004-01-03",
            "2004-01-03",
            "2004-01-04",
            "2004-01-05",
            "2004-01-05",
        ];
        let posts: Vec<_> = dates
            .iter()
            .enumerate()
            .map(|(id, date)| Post {
                id: id as i64,
                source_key: id.to_string(),
                date: (*date).to_owned(),
                words: 10,
                sha256: String::new(),
            })
            .collect();
        let splits = split_dates(&posts).unwrap();
        assert_eq!(splits.len(), 5);
        assert_eq!(splits["2004-01-01"], "train");
        assert_eq!(splits["2004-01-03"], "train");
        assert_eq!(splits["2004-01-04"], "dev");
        assert_eq!(splits["2004-01-05"], "test");
        let post_splits: Vec<_> = posts.iter().map(|post| splits[&post.date]).collect();
        assert_eq!(
            post_splits,
            [
                "train", "train", "train", "train", "train", "dev", "test", "test"
            ]
        );
    }
}

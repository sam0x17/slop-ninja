use crate::{
    DUPLICATE_POLICY, DatePolicy, SCHEMA, TOKENIZER, blog, file_hash, hash, new_directory,
    write_json,
};
use anyhow::{Context, Result, ensure};
use grammar_core::features::words;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};
use zip::ZipArchive;

const MAX_ENTRY_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Summary {
    pub schema: String,
    pub archive_sha256: String,
    pub entries: u64,
    pub entry_status_counts: BTreeMap<String, u64>,
    pub post_status_counts: BTreeMap<String, u64>,
    pub authors: u64,
    pub authors_with_retained_posts: u64,
    pub retained_posts: u64,
    pub retained_words: u64,
    pub unique_words: u64,
    pub retained_posts_with_placeholder: u64,
    pub retained_posts_with_c1_controls: u64,
    pub minimum_retained_date: Option<String>,
    pub maximum_retained_date: Option<String>,
    pub tokenizer: String,
    pub duplicate_policy: String,
    #[serde(default = "DatePolicy::legacy")]
    pub date_policy: DatePolicy,
    pub original_writing_date_minimum: Option<String>,
    pub quotation_and_copying: String,
}

fn schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "PRAGMA journal_mode=DELETE;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE entries (
           id INTEGER PRIMARY KEY, zip_index INTEGER NOT NULL, name TEXT NOT NULL,
           raw_sha256 TEXT NOT NULL, raw_bytes INTEGER NOT NULL,
           encoding TEXT, status TEXT NOT NULL
         );
         CREATE TABLE authors (
           author_id TEXT PRIMARY KEY, retained_posts INTEGER NOT NULL DEFAULT 0,
           total_words INTEGER NOT NULL DEFAULT 0
         ) WITHOUT ROWID;
         CREATE TABLE posts (
           id INTEGER PRIMARY KEY, source_key TEXT NOT NULL UNIQUE,
           entry_id INTEGER NOT NULL REFERENCES entries(id),
           author_id TEXT NOT NULL REFERENCES authors(author_id), ordinal INTEGER NOT NULL,
           date_raw TEXT, authored_date TEXT,
           raw_start INTEGER NOT NULL, raw_end INTEGER NOT NULL,
           body_start INTEGER NOT NULL, body_end INTEGER NOT NULL,
           trimmed_start INTEGER, trimmed_end INTEGER,
           raw_sha256 TEXT NOT NULL, raw_body_sha256 TEXT NOT NULL,
           exact_text_sha256 TEXT, normalized_sha256 TEXT,
           status TEXT NOT NULL, duplicate_of INTEGER REFERENCES posts(id),
           has_placeholder INTEGER NOT NULL, has_c1_controls INTEGER NOT NULL,
           word_count INTEGER NOT NULL, exact_text TEXT
         );
         CREATE UNIQUE INDEX retained_normalized_hash ON posts(normalized_sha256)
           WHERE status='retained';
         CREATE INDEX posts_author ON posts(author_id, status, authored_date, source_key);
         CREATE TABLE author_words (
           author_id TEXT NOT NULL REFERENCES authors(author_id), word TEXT NOT NULL,
           occurrences INTEGER NOT NULL, PRIMARY KEY(author_id,word)
         ) WITHOUT ROWID;
         CREATE TABLE corpus_words (
           word TEXT PRIMARY KEY, occurrences INTEGER NOT NULL
         ) WITHOUT ROWID;",
    )?;
    Ok(())
}

fn bump(counts: &mut BTreeMap<String, u64>, key: &str) {
    *counts.entry(key.to_owned()).or_default() += 1;
}

pub fn import_blog(source: &Path, out: &Path) -> Result<Summary> {
    import_blog_with_date_policy(source, out, DatePolicy::default())
}

pub fn import_blog_with_date_policy(
    source: &Path,
    out: &Path,
    date_policy: DatePolicy,
) -> Result<Summary> {
    let archive_sha256 = file_hash(source)?;
    let mut archive = ZipArchive::new(File::open(source)?)?;
    // Index by literal name and archive index, never by a path extracted to disk.
    let mut order = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        order.push((entry.name().to_owned(), index));
    }
    order.sort();
    new_directory(out)?;
    let db_path = out.join("corpus.sqlite");
    File::create_new(&db_path)?;
    let mut db = Connection::open(&db_path)?;
    schema(&db)?;
    let tx = db.transaction()?;
    let source_manifest = json!({
        "schema": SCHEMA,
        "archive_path": source.canonicalize()?,
        "archive_sha256": archive_sha256,
        "archive_bytes": source.metadata()?.len(),
        "source_url": "https://u.cs.biu.ac.il/~koppel/blogs/blogs.zip",
        "description_url": "https://u.cs.biu.ac.il/~koppel/BlogCorpus.htm",
        "collection": "Blog Authorship Corpus, collected August 2004",
        "license_scope": "local noncommercial research; no corpus redistribution",
        "date_policy": {"version": date_policy.identity(), "minimum_inclusive": date_policy.minimum(), "missing_or_malformed": "reject", "basis": "each post's supplied composition date"},
        "encoding": {
            "default": "iso-8859-1-byte-to-unicode-v1",
            "evidence": "https://huggingface.co/datasets/barilan/blog_authorship_corpus/raw/main/blog_authorship_corpus.py",
            "explicit_utf8": "strict decoding only",
            "other_declarations": "reject entry",
            "c1_controls": "retain and flag; excluded from default author export"
        },
        "text_policy": "trim outer Unicode whitespace; preserve internal whitespace and punctuation; no entity decoding or prose repair",
        "duplicate_policy": DUPLICATE_POLICY,
        "duplicate_winner": "within one author: first eligible occurrence in bytewise entry-name, ZIP-index, post-ordinal order; groups spanning multiple authors: exclude every copy and its counts",
        "word_policy": {"tokenizer": TOKENIZER, "source": "retained exact decoded text", "placeholders": "included in full corpus counts and flagged; excluded as whole posts from default author export"},
        "quotation_and_copying": "unassessed",
        "source_integrity": "archive SHA-256 plus raw entry SHA-256 and half-open byte spans for every recognized post"
    });
    for (key, value) in [
        ("schema", SCHEMA.to_owned()),
        ("archive_sha256", archive_sha256.clone()),
        ("source", serde_json::to_string(&source_manifest)?),
        ("tokenizer", TOKENIZER.to_owned()),
        ("date_policy", date_policy.identity().to_owned()),
    ] {
        tx.execute("INSERT INTO metadata VALUES (?1,?2)", params![key, value])?;
    }
    let mut entry_status_counts = BTreeMap::new();
    let mut post_status_counts = BTreeMap::new();
    for (position, (name, index)) in order.iter().enumerate() {
        if position.is_multiple_of(1000) {
            eprintln!("Importing entry {position}/{}", order.len());
        }
        let mut entry = archive.by_index(*index)?;
        let entry_id = position as i64 + 1;
        ensure!(
            entry.size() <= MAX_ENTRY_BYTES,
            "entry {name:?} exceeds {MAX_ENTRY_BYTES} bytes; aborting without truncation"
        );
        let mut bytes = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("read ZIP entry {name:?}"))?;
        let author = blog::author_id(name);
        let encoding = blog::encoding(&bytes);
        let posts = blog::parse_with_date_policy(&bytes, date_policy);
        let status = if entry.is_dir() {
            "directory"
        } else if author.is_none() {
            "invalid_author_filename"
        } else if encoding.is_none() {
            "unsupported_declared_encoding"
        } else if posts.is_empty() {
            "no_recognized_posts"
        } else {
            "parsed"
        };
        tx.execute(
            "INSERT INTO entries VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                entry_id,
                *index as i64,
                name,
                hash(&bytes),
                bytes.len() as i64,
                encoding.map(blog::Encoding::identity),
                status
            ],
        )?;
        bump(&mut entry_status_counts, status);
        if status != "parsed" {
            continue;
        }
        let author = author.expect("parsed entries have valid authors");
        let encoding = encoding.expect("parsed entries have supported encoding");
        tx.execute(
            "INSERT OR IGNORE INTO authors(author_id) VALUES (?1)",
            [author],
        )?;
        let mut author_words: BTreeMap<String, u64> = BTreeMap::new();
        let mut author_retained_posts = 0_u64;
        let mut author_total_words = 0_u64;
        for post in posts {
            let decoded = blog::text(&bytes, post.body.clone(), encoding);
            let mut status = post.rejection.or(decoded.as_ref().err().copied());
            let text = decoded.ok();
            let mut duplicate_of = None;
            if status.is_none() {
                duplicate_of = tx
                    .query_row(
                        "SELECT id FROM posts WHERE status='retained' AND normalized_sha256=?1",
                        [&text
                            .as_ref()
                            .expect("valid post has text")
                            .normalized_sha256],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                if duplicate_of.is_some() {
                    status = Some("duplicate_text");
                }
            }
            let lexical = text
                .as_ref()
                .map(|text| words(&text.exact))
                .unwrap_or_default();
            let has_placeholder = lexical.iter().any(|word| word == "urllink");
            if status.is_none() && lexical.is_empty() {
                status = Some("no_lexical_words");
            }
            let status = status.unwrap_or("retained");
            if status == "retained" {
                author_retained_posts += 1;
                author_total_words += lexical.len() as u64;
                for word in &lexical {
                    *author_words.entry(word.clone()).or_default() += 1;
                }
            }
            let source_key = format!("{entry_id}:{:06}", post.ordinal);
            tx.prepare_cached(
                "INSERT INTO posts (
                 source_key,entry_id,author_id,ordinal,date_raw,authored_date,
                 raw_start,raw_end,body_start,body_end,trimmed_start,trimmed_end,
                 raw_sha256,raw_body_sha256,exact_text_sha256,normalized_sha256,
                 status,duplicate_of,has_placeholder,has_c1_controls,word_count,exact_text)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"
            )?.execute(params![
                source_key, entry_id, author, post.ordinal as i64, post.date_raw, post.date,
                post.raw.start as i64, post.raw.end as i64,
                post.body.start as i64, post.body.end as i64,
                text.as_ref().map(|text| text.trimmed_span.start as i64),
                text.as_ref().map(|text| text.trimmed_span.end as i64),
                hash(&bytes[post.raw]), hash(&bytes[post.body]),
                text.as_ref().map(|text| &text.exact_sha256),
                text.as_ref().map(|text| &text.normalized_sha256),
                status, duplicate_of, has_placeholder,
                text.as_ref().is_some_and(|text| text.has_c1_controls),
                lexical.len() as i64, text.as_ref().map(|text| &text.exact)
            ])?;
            bump(&mut post_status_counts, status);
        }
        tx.execute(
            "UPDATE authors SET retained_posts=retained_posts+?1,total_words=total_words+?2 WHERE author_id=?3",
            params![author_retained_posts as i64, author_total_words as i64, author],
        )?;
        let mut insert = tx.prepare_cached(
            "INSERT INTO author_words VALUES (?1,?2,?3)
             ON CONFLICT(author_id,word) DO UPDATE SET occurrences=occurrences+excluded.occurrences"
        )?;
        for (word, count) in author_words {
            insert.execute(params![author, word, count as i64])?;
        }
    }
    eprintln!("Excluding all copies of cross-author duplicate groups");
    tx.execute_batch(
        "CREATE TEMP TABLE cross_author_duplicates AS
         SELECT normalized_sha256 FROM posts
         WHERE status IN ('retained','duplicate_text')
         GROUP BY normalized_sha256 HAVING COUNT(DISTINCT author_id)>1;
         CREATE UNIQUE INDEX cross_author_hash ON cross_author_duplicates(normalized_sha256);",
    )?;
    // Retract only the previous winner's contribution; later duplicates were
    // never counted. Query rows in stable source order for reproducible updates.
    {
        let mut winners = tx.prepare(
            "SELECT author_id,exact_text,word_count FROM posts
             WHERE status='retained' AND normalized_sha256 IN
               (SELECT normalized_sha256 FROM cross_author_duplicates)
             ORDER BY id",
        )?;
        let rows = winners.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })?;
        let mut subtract = tx.prepare_cached(
            "UPDATE author_words SET occurrences=occurrences-?1 WHERE author_id=?2 AND word=?3",
        )?;
        for row in rows {
            let (author, text, word_count) = row?;
            let mut counts: BTreeMap<String, u64> = BTreeMap::new();
            for word in words(&text) {
                *counts.entry(word).or_default() += 1;
            }
            ensure!(
                counts.values().sum::<u64>() == word_count,
                "duplicate word count drift"
            );
            for (word, count) in counts {
                ensure!(
                    subtract.execute(params![count as i64, author, word])? == 1,
                    "missing word while retracting cross-author duplicate"
                );
            }
            tx.execute(
                "UPDATE authors SET retained_posts=retained_posts-1,total_words=total_words-?1 WHERE author_id=?2",
                params![word_count as i64, author]
            )?;
        }
    }
    tx.execute_batch(
        "UPDATE posts SET status='cross_author_duplicate'
         WHERE status IN ('retained','duplicate_text') AND normalized_sha256 IN
           (SELECT normalized_sha256 FROM cross_author_duplicates);
         DELETE FROM author_words WHERE occurrences=0;",
    )?;
    let invalid_counts: u64 = tx.query_row(
        "SELECT COUNT(*) FROM author_words WHERE occurrences<0",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        invalid_counts == 0,
        "negative word count after duplicate retraction"
    );
    post_status_counts.clear();
    {
        let mut counts =
            tx.prepare("SELECT status,COUNT(*) FROM posts GROUP BY status ORDER BY status")?;
        for row in counts.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })? {
            let (status, count) = row?;
            post_status_counts.insert(status, count);
        }
    }
    eprintln!("Aggregating corpus vocabulary");
    tx.execute(
        "INSERT INTO corpus_words SELECT word,SUM(occurrences) FROM author_words GROUP BY word",
        [],
    )?;
    tx.execute("INSERT INTO metadata VALUES ('complete','true')", [])?;
    tx.commit()?;
    let count = |sql: &str| -> Result<u64> { Ok(db.query_row(sql, [], |row| row.get(0))?) };
    let summary = Summary {
        schema: SCHEMA.to_owned(),
        archive_sha256,
        entries: order.len() as u64,
        entry_status_counts,
        post_status_counts,
        authors: count("SELECT COUNT(*) FROM authors")?,
        authors_with_retained_posts: count("SELECT COUNT(*) FROM authors WHERE retained_posts>0")?,
        retained_posts: count("SELECT COUNT(*) FROM posts WHERE status='retained'")?,
        retained_words: count("SELECT COALESCE(SUM(total_words),0) FROM authors")?,
        unique_words: count("SELECT COUNT(*) FROM corpus_words")?,
        retained_posts_with_placeholder: count(
            "SELECT COUNT(*) FROM posts WHERE status='retained' AND has_placeholder=1",
        )?,
        retained_posts_with_c1_controls: count(
            "SELECT COUNT(*) FROM posts WHERE status='retained' AND has_c1_controls=1",
        )?,
        minimum_retained_date: db.query_row(
            "SELECT MIN(authored_date) FROM posts WHERE status='retained'",
            [],
            |row| row.get(0),
        )?,
        maximum_retained_date: db.query_row(
            "SELECT MAX(authored_date) FROM posts WHERE status='retained'",
            [],
            |row| row.get(0),
        )?,
        tokenizer: TOKENIZER.to_owned(),
        duplicate_policy: DUPLICATE_POLICY.to_owned(),
        date_policy,
        original_writing_date_minimum: date_policy.minimum().map(str::to_owned),
        quotation_and_copying: "unassessed".to_owned(),
    };
    ensure!(
        count("SELECT COALESCE(SUM(occurrences),0) FROM corpus_words")? == summary.retained_words,
        "corpus and author word denominators disagree"
    );
    write_json(&out.join("source.json"), &source_manifest)?;
    write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

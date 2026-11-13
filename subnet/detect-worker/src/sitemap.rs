//! Sitemap shards of public pages.
//!
//! A public page that goes live takes the next `sitemap_seq`, and shard `n`
//! holds the pages with `n × SHARD_URLS <= sitemap_seq < (n + 1) ×
//! SHARD_URLS`. A shard lists a page only while it is live, newest of its
//! source (not superseded), a web page (never an X or Bluesky post, never a
//! secret page), and indexable by the display policy's source rule or the
//! operator's `index_force = on`; `index_force = off` and `DETECT_INDEX`
//! off leave it out. Every listed URL is a 7-character public slug under the
//! site origin. The hourly cron rebuilds dirty shards into R2
//! (`sitemap/detect-<n>.xml`); the website serves them with its own index.

use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::slug::{self, PUBLIC_LEN};

use crate::db::{Io, IoError, Stmt, Val};

/// URLs per shard: far under the protocol's 50,000, so one shard's rows
/// and XML stay a few hundred KiB in one invocation's arena.
pub const SHARD_URLS: u64 = 5000;
/// Dirty shards rebuilt per cron run (each is one D1 read and one R2 put).
pub const SHARDS_PER_RUN: u64 = 20;

/// A shard's R2 key.
pub fn shard_key(shard: u32) -> String {
    format!("sitemap/detect-{shard}.xml")
}

/// The pages of one shard.
pub const SELECT_SHARD: &str = "SELECT d.public_slug AS public_slug, d.updated_at AS updated_at \
FROM detections d LEFT JOIN sources s ON s.source_id = d.source_id \
WHERE d.sitemap_seq >= ?1 AND d.sitemap_seq < ?2 AND d.kind = 'public_url' AND d.platform = 'web' \
AND d.state = 'live' AND d.superseded_by IS NULL AND d.public_slug IS NOT NULL \
AND (d.index_force = 'on' OR (d.index_force IS NULL AND s.state = 'ok' AND s.reputation = 'ranked')) \
ORDER BY d.sitemap_seq";

/// Dirty shards.
pub const SELECT_DIRTY: &str = "SELECT shard FROM sitemap_shards WHERE dirty = 1 ORDER BY shard LIMIT ?1";

/// Marks a shard built.
pub const MARK_BUILT: &str = "UPDATE sitemap_shards SET dirty = 0, urls = ?2, built_at = ?3 WHERE shard = ?1";

/// Marks a shard dirty (the `sitemap_dirty` message).
pub const MARK_DIRTY: &str = "INSERT INTO sitemap_shards (shard, dirty, urls, built_at) VALUES (?1, 1, 0, NULL) \
ON CONFLICT (shard) DO UPDATE SET dirty = 1";

/// `YYYY-MM-DD` of a time in ms.
fn date(ms: u64) -> String {
    slop_ninja_detect_page::fmt::iso_date(ms)
}

/// The XML of absolute URLs and their last change.
pub fn xml(entries: &[(String, u64)]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for (url, at) in entries {
        // URLs are an origin from config and a slug of the base32 alphabet:
        // nothing in them needs escaping, and a test checks the alphabet.
        s.push_str("<url><loc>");
        s.push_str(url);
        s.push_str("</loc><lastmod>");
        s.push_str(&date(*at));
        s.push_str("</lastmod></url>\n");
    }
    s.push_str("</urlset>\n");
    s
}

/// Rebuilds up to [`SHARDS_PER_RUN`] dirty shards; returns how many.
pub async fn rebuild(io: &impl Io, origins: &Origins, index_on: bool) -> Result<u64, IoError> {
    let dirty = io.run(Stmt::new(SELECT_DIRTY, vec![Val::int(SHARDS_PER_RUN)])).await?;
    let mut n = 0;
    for r in &dirty.rows {
        let Some(shard) = r.u64("shard") else { continue };
        let Ok(shard32) = u32::try_from(shard) else { continue };
        let mut entries = Vec::new();
        if index_on {
            let from = shard.saturating_mul(SHARD_URLS);
            let rows = io
                .run(Stmt::new(SELECT_SHARD, vec![Val::int(from), Val::int(from.saturating_add(SHARD_URLS))]))
                .await?;
            for x in &rows.rows {
                let (Some(slug_), Some(at)) = (x.text("public_slug"), x.u64("updated_at")) else { continue };
                if slug::is_canonical(slug_, PUBLIC_LEN) {
                    entries.push((origins.public_page_url(slug_), at));
                }
            }
        }
        io.doc_put(&shard_key(shard32), xml(&entries).into_bytes()).await?;
        io.run(Stmt::new(MARK_BUILT, vec![Val::int(shard), Val::int(entries.len() as u64), Val::int(io.now_ms())]))
            .await?;
        n += 1;
    }
    Ok(n)
}

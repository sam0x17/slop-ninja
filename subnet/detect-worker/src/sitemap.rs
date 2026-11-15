//! Sitemap shards of public pages.
//!
//! A public page that goes live takes the next `sitemap_seq` (a web page, a
//! Bluesky post or a post on X), and shard `n` holds the pages with `n ×
//! SHARD_URLS <= sitemap_seq < (n + 1) × SHARD_URLS`. A shard lists a page
//! only while it is newest of its source (not superseded; an older page
//! keeps `rel=canonical` to the newest) and indexed: it passes the index
//! gates ([`crate::index_gates`], the same rules here in SQL, read with the
//! same switches a page read uses). `index_force = off` and `DETECT_INDEX`
//! off leave it out. Secret pages never have a position. Every listed URL is
//! a 7-character public slug under the site origin. The hourly cron
//! rebuilds dirty shards into R2 (`sitemap/detect-<n>.xml`); the website
//! serves them with its own index. A rebuild under other switches or
//! another gates version than the last one (`DETECT_INDEX`, `DETECT_X`,
//! [`crate::index_gates::INDEX_GATES_V`], by var or row) first marks every
//! shard dirty, so `DETECT_INDEX` off empties them all.

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

/// The pages of one shard (`?1`..`?2`, the shard's positions): the index
/// gates of [`crate::index_gates`] over the same rows a page read joins.
/// `?3` is `DETECT_X = purged` (a post on X then reads as deleted).
/// Gate 1 reads the stored state alone (no page carries an expiry). Only
/// public pages (`kind = 'public_url'`, a URL check) ever hold a position.
pub fn select_shard() -> String {
    use crate::index_gates::{REPUTATIONS, SOURCE_STATES, sql_in};
    use slop_ninja_detect_page::codes::{Reputation, SourceStatus};
    format!(
        "SELECT d.public_slug AS public_slug, d.updated_at AS updated_at \
FROM detections d JOIN sources s ON s.source_id = d.source_id \
WHERE d.sitemap_seq >= ?1 AND d.sitemap_seq < ?2 AND d.kind = 'public_url' \
AND d.superseded_by IS NULL AND d.public_slug IS NOT NULL \
AND d.state = 'live' AND NOT (d.platform = 'x' AND ?3 = 1) \
AND s.state IN ({states}) \
AND NOT EXISTS (SELECT 1 FROM optouts o WHERE o.host = s.host) \
AND (d.index_force IS NULL OR d.index_force = 'on') \
AND (s.reputation IN ({reputations}) OR d.index_force = 'on') \
ORDER BY d.sitemap_seq",
        states = sql_in(SOURCE_STATES, SourceStatus::as_str),
        reputations = sql_in(REPUTATIONS, Reputation::as_str),
    )
}

/// The `schema_info` key of what the shards were last built under.
pub const BUILT_UNDER_KEY: &str = "sitemap_built_under";

/// What a shard's rows depend on besides the pages: the gates' version and
/// the switches they read.
pub fn built_under(sw: &crate::display::PageSwitches) -> String {
    format!("{} index={} x_purged={}", crate::index_gates::INDEX_GATES_V, sw.index, sw.x_purged)
}

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

/// Rebuilds up to [`SHARDS_PER_RUN`] dirty shards; returns how many. The
/// switches are read once, as a page read reads them (var AND row): with
/// `DETECT_INDEX` off every shard rebuilds empty. When the switch rows
/// cannot be read nothing is rebuilt (a page read fails then too), so a
/// row's off is never read past. A change of [`built_under`] marks every
/// shard first.
pub async fn rebuild(io: &impl Io, origins: &Origins) -> Result<u64, IoError> {
    let rows = io.run(Stmt::new(crate::lookup::SELECT_SWITCHES, Vec::new())).await?;
    let sw = crate::lookup::SwitchRows::from_done(Some(&rows)).page(io);
    let under = built_under(&sw);
    let last = io.run(Stmt::new("SELECT v FROM schema_info WHERE k = ?1", vec![Val::text(BUILT_UNDER_KEY)])).await?;
    if last.first().and_then(|r| r.text("v")) != Some(under.as_str()) {
        io.batch(vec![
            Stmt::new("UPDATE sitemap_shards SET dirty = 1", Vec::new()),
            Stmt::new(
                "INSERT INTO schema_info (k, v) VALUES (?1, ?2) ON CONFLICT (k) DO UPDATE SET v = excluded.v",
                vec![Val::text(BUILT_UNDER_KEY), Val::text(under)],
            ),
        ])
        .await?;
    }
    let dirty = io.run(Stmt::new(SELECT_DIRTY, vec![Val::int(SHARDS_PER_RUN)])).await?;
    let select = select_shard();
    let mut n = 0;
    for r in &dirty.rows {
        let Some(shard) = r.u64("shard") else { continue };
        let Ok(shard32) = u32::try_from(shard) else { continue };
        let mut entries = Vec::new();
        if sw.index {
            let from = shard.saturating_mul(SHARD_URLS);
            let rows = io
                .run(Stmt::owned(
                    select.clone(),
                    vec![Val::int(from), Val::int(from.saturating_add(SHARD_URLS)), Val::int(u64::from(sw.x_purged))],
                ))
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

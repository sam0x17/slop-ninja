//! The page cache's purges (natively tested).
//!
//! The website and the secret host keep a rendered page, a 410 and a 451 in
//! their page cache (the Cache API, per data center) for up to
//! `PAGE_CACHE_S`. A copy must not outlive a change the reader has to see
//! at once, so every copy of a stored page carries the page's cache tag and
//! the store purges the tag when such a change lands.
//!
//! - **The tag** ([`cache_tag`]): [`TAG_PREFIX`] and the first 16 bytes, in
//!   hex, of `H("slop_ninja_detect_cache_tag_v1", id_h)`. Every page and
//!   card answer of a stored page carries it (`Page::cache_tag`,
//!   `Card::cache_tag`), and the website stores each copy of the page, its
//!   X widget route and its card under it (`Cache-Tag`), so one purge
//!   reaches them all, on both hosts and in every data center. It is a hash
//!   of the page's store id, itself a keyed hash of a secret slug, so it
//!   names neither the slug nor anything of the text, and it is never sent
//!   to a reader.
//! - **When** ([`purge_pages`]): a removal (a case's, the operator's, an X
//!   removal), a restore, a case's disable, the other steps of a case that
//!   change what a page shows (a withheld excerpt, a reject that returns a
//!   page), a page its owner deletes (the manage page, the account's list,
//!   the account's deletion) and the operator's `delete_private`. The purge
//!   runs in the invocation that made the change, once the change is
//!   stored, through the zone's purge by tag (`CACHE_PURGE_TOKEN` and
//!   `CACHE_PURGE_ZONE`). Without them nothing is purged and the cache's
//!   lifetime bounds the delay; a purge the API refuses writes
//!   `error {purge}`.
//! - **Not purged: switch flips.** The website reads `DETECT_PAGES` (the
//!   emergency switch) before it serves any cached page
//!   (`/internal/v1/pages`); the other switches reach a page already in the
//!   cache within `PAGE_CACHE_S`.

use crate::db::Io;
use crate::keys::{Bytes32, h};

/// The tag's domain.
pub const CACHE_TAG_DOMAIN: &[u8] = b"slop_ninja_detect_cache_tag_v1";
/// The tag's prefix (its version).
pub const TAG_PREFIX: &str = "snp1-";
/// The most tags one purge call names.
pub const TAGS_PER_PURGE: usize = 30;

/// The cache tag of the page whose store id is `id_h`.
pub fn cache_tag(id_h: &Bytes32) -> String {
    let d = h(CACHE_TAG_DOMAIN, id_h);
    let mut s = String::with_capacity(TAG_PREFIX.len() + 32);
    s.push_str(TAG_PREFIX);
    s.push_str(&hex::encode(d.get(..16).unwrap_or_default()));
    s
}

/// Purges the cached copies of these pages (each id once), in calls of at
/// most [`TAGS_PER_PURGE`] tags. Best effort: a failed call writes
/// `error {purge}` and the next goes on; nothing is retried.
pub async fn purge_pages(io: &impl Io, ids: &[Bytes32]) {
    let mut tags: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        let t = cache_tag(id);
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    for chunk in tags.chunks(TAGS_PER_PURGE) {
        if io.cache_purge_tags(chunk).await.is_err() {
            io.metric_n("error", &["purge"], chunk.len() as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_is_a_versioned_hash_of_the_store_id() {
        let a = cache_tag(&[7u8; 32]);
        assert_eq!(a.len(), TAG_PREFIX.len() + 32);
        assert!(a.starts_with(TAG_PREFIX));
        assert!(a[TAG_PREFIX.len()..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(a, cache_tag(&[7u8; 32]), "stable");
        assert_ne!(a, cache_tag(&[8u8; 32]));
        // Not the id itself, nor its plain prefix.
        assert!(!a.contains(&hex::encode([7u8; 16])));
        // The tag form the zone's purge takes: letters, digits and hyphens.
        assert!(a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    }
}

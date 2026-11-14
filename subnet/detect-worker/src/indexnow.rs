//! IndexNow pings for public pages: when a page is indexed, changes or is
//! removed, the `indexnow {public_slug, change}` message tells the search
//! engines that take IndexNow (one POST to `api.indexnow.org`, which shares
//! it with the others).
//!
//! - `updated`: pinged only while the page is indexed (its page JSON says
//!   `index`); a page that fails a gate is never announced.
//! - `deleted`: always pinged, so a removed or deleted page is crawled again
//!   and dropped.
//!
//! With no key (`INDEXNOW_KEY` unset) every message is dropped: the site has
//! no key file, and the sitemaps carry the change anyway. The key file
//! itself (`/<key>.txt` on the site, holding the key) is the website's.
//! Posts on X and Bluesky are never indexed, so they are never pinged.

use serde::Serialize;
use slop_ninja_detect_page::codes::{IndexChange, Robots};
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_detect_page::slug::{self, PUBLIC_LEN};

/// The shared IndexNow endpoint.
pub const ENDPOINT: &str = "https://api.indexnow.org/indexnow";
/// The key's var (a secret: 8 to 128 characters of `[A-Za-z0-9-]`).
pub const KEY_VAR: &str = "INDEXNOW_KEY";

/// Whether `key` has IndexNow's form.
pub fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Body<'a> {
    host: &'a str,
    key: &'a str,
    key_location: String,
    url_list: Vec<String>,
}

/// The ping's body for the public pages `slugs`; `None` with no valid key
/// or no canonical public slug.
pub fn ping_body(origins: &Origins, key: &str, slugs: &[&str]) -> Option<Vec<u8>> {
    if !valid_key(key) {
        return None;
    }
    let urls: Vec<String> =
        slugs.iter().filter(|s| slug::is_canonical(s, PUBLIC_LEN)).map(|s| origins.public_page_url(s)).collect();
    if urls.is_empty() {
        return None;
    }
    let site = origins.of(slop_ninja_detect_page::codes::HostRole::Site);
    let body = Body { host: site.host(), key, key_location: site.url(&format!("/{key}.txt")), url_list: urls };
    serde_json::to_vec(&body).ok()
}

/// Whether a change is announced, given the page's robots directive now
/// (`None` when the page no longer reads).
pub fn announce(change: IndexChange, robots: Option<Robots>) -> bool {
    match change {
        IndexChange::Deleted => true,
        IndexChange::Updated => robots == Some(Robots::Index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origins() -> Origins {
        Origins::from_vars("https://site.example", "https://s.site.example").unwrap()
    }

    #[test]
    fn the_body_names_the_page_and_the_key_file() {
        let b = ping_body(&origins(), "abcdef0123456789", &["k7m2xq9"]).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!(v["host"], "site.example");
        assert_eq!(v["key"], "abcdef0123456789");
        assert_eq!(v["keyLocation"], "https://site.example/abcdef0123456789.txt");
        assert_eq!(v["urlList"], serde_json::json!(["https://site.example/d/k7m2xq9"]));
        assert_eq!(ping_body(&origins(), "short", &["k7m2xq9"]), None);
        assert_eq!(ping_body(&origins(), "abcdef0123456789", &["7hq2kd9m4xw8rt3vn6pb"]), None);
    }

    #[test]
    fn only_indexed_pages_are_announced_and_every_removal_is() {
        assert!(announce(IndexChange::Updated, Some(Robots::Index)));
        assert!(!announce(IndexChange::Updated, Some(Robots::Noindex)));
        assert!(!announce(IndexChange::Updated, None));
        assert!(announce(IndexChange::Deleted, None));
    }
}

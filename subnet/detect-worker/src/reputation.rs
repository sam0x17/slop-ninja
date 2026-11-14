//! The ranking shards: whether a registrable domain is in the top-domain
//! ranking (Cloudflare Radar's top-1,000,000 bucket), read without loading
//! the whole list.
//!
//! The weekly job writes one list version under `rank/<v>/`: 256 shards
//! `rank/<v>/<xx>`, `xx` the first byte of `SHA-256(domain)` in hex, each the
//! shard's domains sorted, one per line, lowercase A-labels. A lookup reads
//! one shard (about 4,000 domains, some 60 KB) and searches it. The current
//! version's name is in `rank/current` (`<v>\n`). With no list yet every
//! domain is `unranked`: no title, no excerpt, `noindex`, the safe reading.
//!
//! The family screen (1.1.1.1 for Families) then marks adult domains; the
//! decision is [`crate::host::decide_reputation`].

use sha2::{Digest, Sha256};

/// The R2 prefix of the ranking lists.
pub const RANK_PREFIX: &str = "rank/";
/// The key naming the current list version.
pub const RANK_CURRENT: &str = "rank/current";
/// The ranking list version of a domain with no list (`sources.rank_list_v`).
pub const NO_LIST: &str = "none";
/// Shards per list.
pub const SHARDS: usize = 256;
/// The longest domain a shard holds (DNS's limit).
pub const MAX_DOMAIN: usize = 253;

/// The shard of a domain.
pub fn shard_of(domain: &str) -> u8 {
    Sha256::digest(domain.as_bytes()).first().copied().unwrap_or(0)
}

/// The key of shard `n` of list `v`.
pub fn shard_key(v: &str, n: u8) -> String {
    format!("{RANK_PREFIX}{v}/{n:02x}")
}

/// Whether a list version name is safe as a key part: `[a-z0-9-]{1,40}`.
pub fn valid_version(v: &str) -> bool {
    !v.is_empty() && v.len() <= 40 && v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The current version from `rank/current`'s body.
pub fn current_version(body: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?.trim();
    valid_version(s).then(|| s.to_owned())
}

/// A domain as the lists hold it: trimmed, lowercase, no trailing dot;
/// `None` for anything that cannot be a domain.
pub fn normal(domain: &str) -> Option<String> {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = !d.is_empty()
        && d.len() <= MAX_DOMAIN
        && d.contains('.')
        && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    ok.then_some(d)
}

/// Splits a ranking list (one domain per line; a CSV whose first column is
/// the domain, its header skipped) into its shards, each sorted and
/// deduplicated.
pub fn build_shards<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Vec<String>> {
    let mut shards: Vec<Vec<String>> = vec![Vec::new(); SHARDS];
    for line in lines {
        let first = line.split(',').next().unwrap_or_default();
        let Some(d) = normal(first.trim_matches('"')) else { continue };
        if let Some(s) = shards.get_mut(usize::from(shard_of(&d))) {
            s.push(d);
        }
    }
    for s in &mut shards {
        s.sort_unstable();
        s.dedup();
    }
    shards
}

/// A shard's body.
pub fn shard_body(domains: &[String]) -> Vec<u8> {
    let mut out = Vec::with_capacity(domains.iter().map(|d| d.len() + 1).sum());
    for d in domains {
        out.extend_from_slice(d.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Whether `domain` is in a shard's body (sorted lines; binary search over
/// line starts).
pub fn in_shard(body: &[u8], domain: &str) -> bool {
    let Some(d) = normal(domain) else { return false };
    let starts: Vec<usize> = std::iter::once(0)
        .chain(body.iter().enumerate().filter(|(_, b)| **b == b'\n').map(|(i, _)| i + 1))
        .filter(|i| *i < body.len())
        .collect();
    let line = |i: usize| -> &[u8] {
        let s = starts.get(i).copied().unwrap_or(0);
        let rest = body.get(s..).unwrap_or_default();
        let end = rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len());
        rest.get(..end).unwrap_or_default()
    };
    let (mut lo, mut hi) = (0usize, starts.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match line(mid).cmp(d.as_bytes()) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards_hold_every_domain_once_and_find_it() {
        let csv = "domain,rank\nexample.com,1\nEXAMPLE.org.,2\nexample.com,3\nnot a domain,4\n\"news.example\",5\nlocalhost,6";
        let shards = build_shards(csv.lines());
        assert_eq!(shards.len(), SHARDS);
        let all: Vec<&String> = shards.iter().flatten().collect();
        assert_eq!(all.len(), 3, "{all:?}");
        for d in ["example.com", "example.org", "news.example"] {
            let s = &shards[usize::from(shard_of(d))];
            assert!(in_shard(&shard_body(s), d), "{d}");
            assert!(in_shard(&shard_body(s), &d.to_uppercase()), "{d}");
        }
        assert!(!in_shard(&shard_body(&shards[usize::from(shard_of("absent.example"))]), "absent.example"));
        assert!(!in_shard(b"", "example.com"));
    }

    #[test]
    fn binary_search_over_a_large_shard() {
        let mut v: Vec<String> = (0..5000).map(|i| format!("d{i:05}.example")).collect();
        v.sort();
        let body = shard_body(&v);
        for i in [0, 1, 2499, 4998, 4999] {
            assert!(in_shard(&body, &format!("d{i:05}.example")));
        }
        assert!(!in_shard(&body, "d05000.example") && !in_shard(&body, "a.example") && !in_shard(&body, "z.example"));
    }

    #[test]
    fn versions_and_keys() {
        assert_eq!(current_version(b"2026-11-09\n"), Some("2026-11-09".into()));
        assert_eq!(current_version(b"../x"), None);
        assert_eq!(shard_key("2026-11-09", 0x0a), "rank/2026-11-09/0a");
    }
}

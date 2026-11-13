//! The Public Suffix List, compiled into a sorted table by `build.rs`
//! (`psl/public_suffix_list.dat`, MPL-2.0; its `VERSION` and SHA-256 are
//! [`PSL_LIST_VERSION`] and [`PSL_SHA256`]). Both the ICANN and the private
//! sections are used, so `user.github.io` is its own registrable domain.
//!
//! Lookups follow the list's algorithm (publicsuffix.org/list): the
//! matching rule with the most labels prevails, an exception rule prevails
//! over all and drops its leftmost label, and the implicit `*` rule makes
//! an unlisted top-level label a public suffix. Hosts must be the A-label,
//! lowercase form the URL parser produces. Nothing here allocates.

include!(concat!(env!("OUT_DIR"), "/psl_rules.rs"));

const NORMAL: u8 = 1;
const WILDCARD: u8 = 2;
const EXCEPTION: u8 = 4;

/// The table's version: the list's own version line and the file digest.
pub fn version() -> (&'static str, &'static str) {
    (PSL_LIST_VERSION, PSL_SHA256)
}

fn flags(suffix: &str) -> u8 {
    match RULES.binary_search_by(|(k, _)| (*k).cmp(suffix)) {
        Ok(i) => RULES.get(i).map_or(0, |r| r.1),
        Err(_) => 0,
    }
}

/// Whether some rule names this top-level label.
pub fn known_tld(tld: &str) -> bool {
    TLDS.binary_search(&tld).is_ok()
}

/// Byte offsets where each label of `host` starts, right to left: the
/// suffix of `k` labels is `&host[starts[k - 1]..]`.
fn label_start(host: &str, k: usize) -> Option<usize> {
    if k == 0 {
        return Some(host.len());
    }
    let mut seen = 0;
    for (i, b) in host.bytes().enumerate().rev() {
        if b == b'.' {
            seen += 1;
            if seen == k {
                return Some(i + 1);
            }
        }
    }
    (seen + 1 == k).then_some(0)
}

/// The number of labels of the public suffix of `host`.
fn suffix_labels(host: &str) -> usize {
    let n = host.split('.').count();
    let mut best = 1;
    let mut exception = None;
    for k in 1..=n {
        let Some(s) = label_start(host, k).and_then(|i| host.get(i..)) else { break };
        let f = flags(s);
        if f & EXCEPTION != 0 {
            exception = Some(k);
        }
        if f & NORMAL != 0 {
            best = best.max(k);
        }
        if k >= 2
            && let Some(parent) = label_start(host, k - 1).and_then(|i| host.get(i..))
            && flags(parent) & WILDCARD != 0
        {
            best = best.max(k);
        }
    }
    match exception {
        Some(k) => k - 1,
        None => best,
    }
}

/// The public suffix of `host`.
pub fn public_suffix(host: &str) -> Option<&str> {
    host.get(label_start(host, suffix_labels(host))?..)
}

/// The registrable domain (the public suffix plus one label), or `None`
/// when `host` is itself a public suffix.
pub fn registrable_domain(host: &str) -> Option<&str> {
    let k = suffix_labels(host);
    if host.split('.').count() <= k {
        return None;
    }
    host.get(label_start(host, k + 1)?..)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registrable_domains() {
        for (host, want) in [
            ("example.com", Some("example.com")),
            ("www.example.com", Some("example.com")),
            ("a.b.example.co.uk", Some("example.co.uk")),
            ("co.uk", None),
            ("com", None),
            ("user.github.io", Some("user.github.io")),
            ("x.user.github.io", Some("user.github.io")),
            ("github.io", None),
            ("www.ck", Some("www.ck")),
            ("a.b.ck", Some("a.b.ck")),
            ("x.a.b.ck", Some("a.b.ck")),
            ("b.ck", None),
            ("xn--bcher-kva.de", Some("xn--bcher-kva.de")),
            ("a.notatld", Some("a.notatld")),
            ("bsky.app", Some("bsky.app")),
        ] {
            assert_eq!(registrable_domain(host), want, "{host}");
        }
        assert_eq!(public_suffix("a.b.example.co.uk"), Some("co.uk"));
        assert_eq!(public_suffix("x.user.github.io"), Some("github.io"));
    }

    #[test]
    fn the_table_is_sorted_and_versioned() {
        assert!(RULES.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(TLDS.windows(2).all(|w| w[0] < w[1]));
        assert!(known_tld("com") && known_tld("uk") && known_tld("ninja") && !known_tld("notatld") && !known_tld("corp"));
        let (v, sha) = version();
        assert!(!v.is_empty() && sha.len() == 64);
        // Internationalized rules are stored in A-label form.
        assert!(RULES.iter().all(|(k, _)| k.is_ascii()));
    }
}

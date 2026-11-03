//! Registered domains (eTLD+1) from the Public Suffix List.
//!
//! The list text is read from a downloaded `public_suffix_list.dat` (both
//! ICANN and private sections, the standard eTLD+1 definition), so a
//! `user.blogspot.com` site and a `user.github.io` site are their own
//! registered domains. Rules follow the published algorithm: exception rules
//! (`!`) win, otherwise the matching rule with the most labels; the implicit
//! default rule is `*`. Hosts are lowercased ASCII; punycode (`xn--`) labels
//! are compared as written, so a Unicode-only rule cannot match them and the
//! default rule applies (recorded as a limit).
use std::collections::HashSet;

pub struct Psl {
    rules: HashSet<String>,
    wildcards: HashSet<String>,
    exceptions: HashSet<String>,
    pub rule_count: usize,
}

impl Psl {
    pub fn parse(list: &str) -> Psl {
        let (mut rules, mut wildcards, mut exceptions) = (HashSet::new(), HashSet::new(), HashSet::new());
        let mut n = 0;
        for line in list.lines() {
            let l = line.split_whitespace().next().unwrap_or("");
            if l.is_empty() || l.starts_with("//") {
                continue;
            }
            let l = l.to_lowercase();
            n += 1;
            if let Some(x) = l.strip_prefix('!') {
                exceptions.insert(x.to_owned());
            } else if let Some(x) = l.strip_prefix("*.") {
                wildcards.insert(x.to_owned());
            } else {
                rules.insert(l);
            }
        }
        Psl { rules, wildcards, exceptions, rule_count: n }
    }

    /// Number of labels in the public suffix of `host` (at least 1).
    fn suffix_labels(&self, labels: &[&str]) -> usize {
        let n = labels.len();
        // Exception rules: the suffix is the rule minus its leftmost label.
        for k in (1..=n).rev() {
            let cand = labels[n - k..].join(".");
            if self.exceptions.contains(&cand) {
                return k - 1;
            }
        }
        let mut best = 1;
        for k in 1..=n {
            let cand = labels[n - k..].join(".");
            if self.rules.contains(&cand) {
                best = best.max(k);
            }
            // `*.parent` matches any one label in front of `parent`.
            if k >= 2 && self.wildcards.contains(&labels[n - k + 1..].join(".")) {
                best = best.max(k);
            }
        }
        best
    }

    /// Registered domain of a host, or None for an IP address, an empty
    /// host or a host that is itself a public suffix.
    pub fn registered_domain(&self, host: &str) -> Option<String> {
        let h = host.trim().trim_end_matches('.').to_lowercase();
        if h.is_empty() || h.parse::<std::net::IpAddr>().is_ok() || h.starts_with('[') {
            return None;
        }
        let labels: Vec<&str> = h.split('.').collect();
        if labels.iter().any(|l| l.is_empty()) {
            return None;
        }
        let s = self.suffix_labels(&labels);
        (labels.len() > s).then(|| labels[labels.len() - s - 1..].join("."))
    }
}

/// Lowercased host of an absolute http(s) URL, without userinfo or port.
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r)?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit_once('@').map(|(_, h)| h).unwrap_or(auth);
    let host = if auth.starts_with('[') {
        auth.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        auth.split(':').next().unwrap_or("").to_owned()
    };
    (!host.is_empty()).then(|| host.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    const LIST: &str = "// comment\ncom\nuk\nco.uk\n*.ck\n!www.ck\n// ===BEGIN PRIVATE DOMAINS===\nblogspot.com\ngithub.io\n";
    #[test]
    fn registered_domains_follow_the_psl_algorithm() {
        let p = Psl::parse(LIST);
        assert_eq!(p.rule_count, 7);
        assert_eq!(p.registered_domain("www.example.com").as_deref(), Some("example.com"));
        assert_eq!(p.registered_domain("a.b.example.co.uk").as_deref(), Some("example.co.uk"));
        assert_eq!(p.registered_domain("me.blogspot.com").as_deref(), Some("me.blogspot.com"));
        assert_eq!(p.registered_domain("x.me.github.io").as_deref(), Some("me.github.io"));
        assert_eq!(p.registered_domain("a.b.c.ck").as_deref(), Some("b.c.ck"));
        assert_eq!(p.registered_domain("www.ck").as_deref(), Some("www.ck"));
        assert_eq!(p.registered_domain("blog.example.org").as_deref(), Some("example.org"), "default rule *");
        assert_eq!(p.registered_domain("co.uk"), None);
        assert_eq!(p.registered_domain("10.0.0.1"), None);
        assert_eq!(url_host("https://User@WWW.Example.com:8080/a?b").as_deref(), Some("www.example.com"));
        assert_eq!(url_host("http://example.com").as_deref(), Some("example.com"));
        assert_eq!(url_host("nonsense"), None);
    }
}

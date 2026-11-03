//! Cohort-domain assignment and group keys.
//!
//! Rules v1 (`rules/domain-rules-pilot-v1.json`): URL rules decide first,
//! tier 1 (hosts, host patterns, strong path patterns) before tier 2 (weak
//! path patterns), each in the rules' priority order. Without a URL rule,
//! the domain with the most distinct content cues on the page wins if it
//! has at least `min_cues` (ties by priority); everything else is
//! `general_web`.
//!
//! Rules v2 (`rules/domain-rules-pilot-v2.json`, the default) add
//! per-domain gates that read the chosen excerpt, the evaluation unit
//! (navigation, sidebars and comment widgets supplied most false page cues
//! in the 2026-10-30 precision review). Every option defaults to the v1
//! behaviour, so v1 reproduces:
//! - `min_excerpt_cues`: content-cue assignment also needs this many of the
//!   domain's cues in the excerpt;
//! - `anchors`: content-cue assignment also needs one of these
//!   domain-specific cues in the excerpt (anchors count as cues);
//! - `weak_min_cues` / `weak_min_excerpt_cues` (file defaults, per-domain
//!   overrides): a tier-2 rule (weak host, weak host pattern, weak path)
//!   fires only when its domain has that many cues on the page / in the
//!   excerpt;
//! - `content_before_weak`: gated content cues decide before tier 2.
//!
//! Rules v3 (`rules/domain-rules-pilot-v3.json`, the default) add three
//! options from the 2026-10-31 fresh precision draw, again defaulting to the
//! earlier behaviour so v1 and v2 reproduce:
//! - `content_assign: false` (per domain): the domain is never assigned by
//!   content cues alone; only a URL rule (tier 1, or a gated tier-2 rule)
//!   assigns it. Its cues still gate its weak rules.
//! - `sections`: a document assigned to `from` whose URL path matches one of
//!   `paths` goes to `to` (newspaper opinion and column sections to essays).
//! - `comment_sections`: a document of a `from` domain whose excerpt sits in
//!   the page's comment section goes to `to` (blog and news comments to
//!   forum). The excerpt is located among the page's non-empty lines; it is
//!   in the comment section when one of the `window` lines just before it is
//!   a comment meta line (or its first `inline_chars` characters carry a
//!   comment timestamp) and the page before it holds a comment-count header
//!   or at least `min_meta_before` meta lines. Page-only assignment (pass 1)
//!   has no excerpt, so this step needs pass 2.
//!
//! Each result names its rule.
use anyhow::{Context, Result, ensure};
use regex::bytes::Regex;
use serde::Deserialize;
use std::collections::BTreeMap;

pub const GENERAL: &str = "general_web";
pub const PILOT_RULES_V1: &str = include_str!("../rules/domain-rules-pilot-v1.json");
pub const PILOT_RULES_V2: &str = include_str!("../rules/domain-rules-pilot-v2.json");
pub const PILOT_RULES_V3: &str = include_str!("../rules/domain-rules-pilot-v3.json");
/// The default pilot rules.
pub const PILOT_RULES: &str = PILOT_RULES_V3;

#[derive(Deserialize)]
struct RuleFile {
    version: String,
    priority: Vec<String>,
    min_cues: usize,
    #[serde(default)]
    weak_min_cues: usize,
    #[serde(default)]
    weak_min_excerpt_cues: usize,
    #[serde(default)]
    content_before_weak: bool,
    domains: BTreeMap<String, DomainSpec>,
    multi_author_hosts: Vec<String>,
    #[serde(default)]
    sections: Vec<SectionSpec>,
    #[serde(default)]
    comment_sections: Option<CommentSpec>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct SectionSpec {
    from: String,
    to: String,
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct CommentSpec {
    from: Vec<String>,
    to: String,
    window: usize,
    inline_chars: usize,
    min_meta_before: usize,
    timestamp_max_words: usize,
    headers: Vec<String>,
    meta: Vec<String>,
    timestamp: String,
    not_meta: Vec<String>,
}

#[derive(Deserialize)]
struct DomainSpec {
    hosts: Vec<String>,
    host_patterns: Vec<String>,
    paths: Vec<String>,
    weak_paths: Vec<String>,
    #[serde(default)]
    weak_hosts: Vec<String>,
    #[serde(default)]
    weak_host_patterns: Vec<String>,
    #[serde(default)]
    min_cues: Option<usize>,
    #[serde(default)]
    min_excerpt_cues: usize,
    #[serde(default = "yes")]
    content_assign: bool,
    #[serde(default)]
    weak_min_cues: Option<usize>,
    #[serde(default)]
    weak_min_excerpt_cues: Option<usize>,
    cues: Vec<String>,
    #[serde(default)]
    anchors: Vec<String>,
}

struct Compiled {
    name: String,
    /// (host, optional path prefix) pairs; `whitehouse.gov/briefing-room`.
    hosts: Vec<(String, Option<String>)>,
    host_re: Vec<Regex>,
    paths: Vec<Regex>,
    weak: Vec<Regex>,
    weak_hosts: Vec<(String, Option<String>)>,
    weak_host_re: Vec<Regex>,
    min_cues: usize,
    min_excerpt_cues: usize,
    weak_min_cues: usize,
    weak_min_excerpt_cues: usize,
    has_anchors: bool,
    content_assign: bool,
}

/// A compiled section rule: documents of domain `from` whose path matches
/// `re` go to `to` (domain indices).
struct Section {
    from: usize,
    to: usize,
    re: Regex,
}

/// The compiled comment-section rule.
struct Comments {
    from: Vec<usize>,
    to: usize,
    window: usize,
    inline_chars: usize,
    min_meta_before: usize,
    timestamp_max_words: usize,
    headers: Vec<Regex>,
    meta: Vec<Regex>,
    timestamp: Regex,
    not_meta: Vec<Regex>,
}

impl Comments {
    fn is_header(&self, l: &str) -> bool {
        self.headers.iter().any(|r| r.is_match(l.as_bytes()))
    }
    fn is_meta(&self, l: &str) -> bool {
        let b = l.as_bytes();
        if self.not_meta.iter().any(|r| r.is_match(b)) {
            return false;
        }
        self.meta.iter().any(|r| r.is_match(b))
            || (self.timestamp.is_match(b) && l.split_whitespace().count() <= self.timestamp_max_words)
    }
    /// Whether the excerpt (paragraphs joined by blank lines) sits in the
    /// page's comment section; `None` when it cannot be located.
    fn holds(&self, page: &str, excerpt: &str) -> Option<bool> {
        let lines: Vec<String> = page.split('\n').map(grammar_a_pools::text::squash).filter(|l| !l.is_empty()).collect();
        let paras: Vec<&str> = excerpt.split("\n\n").collect();
        let k = paras.len();
        let start = (0..(lines.len() + 1).saturating_sub(k)).find(|&i| lines[i..i + k].iter().zip(&paras).all(|(a, b)| a == b))?;
        let pre = &lines[..start];
        let win = &pre[pre.len().saturating_sub(self.window)..];
        let first = paras[0];
        let head: String = first.chars().take(self.inline_chars).collect();
        let inline = self.timestamp.is_match(head.as_bytes()) && !self.not_meta.iter().any(|r| r.is_match(first.as_bytes()));
        if !(inline || win.iter().any(|l| self.is_meta(l))) {
            return Some(false);
        }
        Some(pre.iter().any(|l| self.is_header(l)) || pre.iter().filter(|l| self.is_meta(l)).count() >= self.min_meta_before)
    }
}

pub struct Rules {
    pub version: String,
    pub sha256: String,
    pub min_cues: usize,
    pub content_before_weak: bool,
    domains: Vec<Compiled>,
    /// (domain index, is anchor, cue pattern), each run on its own: one
    /// combined set of ~100 patterns thrashes the lazy DFA.
    cue_res: Vec<(usize, bool, Regex)>,
    multi_author: Vec<String>,
    sections: Vec<Section>,
    comments: Option<Comments>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Assignment {
    pub domain: String,
    pub rule: String,
    /// Distinct content cues per domain that fired on the page (sorted by
    /// domain).
    pub cues: BTreeMap<String, usize>,
    /// The same on the excerpt (equal to `cues` for page-only assignment).
    pub excerpt_cues: BTreeMap<String, usize>,
}

fn ci(p: &str) -> Result<Regex> {
    Regex::new(&format!("(?i-u){p}")).with_context(|| format!("pattern {p}"))
}

fn host_matches(host: &str, h: &str) -> bool {
    host == h || (host.len() > h.len() && host.ends_with(h) && host.as_bytes()[host.len() - h.len() - 1] == b'.')
}

fn host_list(v: &[String]) -> Vec<(String, Option<String>)> {
    v.iter()
        .map(|h| match h.split_once('/') {
            Some((a, b)) => (a.to_lowercase(), Some(format!("/{}", b.to_lowercase()))),
            None => (h.to_lowercase(), None),
        })
        .collect()
}

fn host_hit<'a>(list: &'a [(String, Option<String>)], host: &str, path_lc: &str) -> Option<&'a (String, Option<String>)> {
    list.iter()
        .find(|(h, pre)| host_matches(host, h) && pre.as_ref().is_none_or(|p| path_lc.starts_with(p)))
}

impl Rules {
    pub fn parse(text: &str) -> Result<Rules> {
        let f: RuleFile = serde_json::from_str(text).context("domain rules JSON")?;
        let mut domains = Vec::new();
        let mut cue_res = Vec::new();
        for name in &f.priority {
            let d = f.domains.get(name).with_context(|| format!("priority names unknown domain {name}"))?;
            let di = domains.len();
            for c in &d.cues {
                cue_res.push((di, false, ci(c)?));
            }
            for c in &d.anchors {
                cue_res.push((di, true, ci(c)?));
            }
            domains.push(Compiled {
                name: name.clone(),
                hosts: host_list(&d.hosts),
                host_re: d.host_patterns.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                paths: d.paths.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                weak: d.weak_paths.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                weak_hosts: host_list(&d.weak_hosts),
                weak_host_re: d.weak_host_patterns.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                min_cues: d.min_cues.unwrap_or(f.min_cues),
                min_excerpt_cues: d.min_excerpt_cues,
                weak_min_cues: d.weak_min_cues.unwrap_or(f.weak_min_cues),
                weak_min_excerpt_cues: d.weak_min_excerpt_cues.unwrap_or(f.weak_min_excerpt_cues),
                has_anchors: !d.anchors.is_empty(),
                content_assign: d.content_assign,
            });
        }
        ensure!(domains.len() == f.domains.len(), "every domain must appear in priority");
        let index = |name: &str| -> Result<usize> {
            f.priority.iter().position(|p| p == name).with_context(|| format!("unknown domain {name}"))
        };
        let mut sections = Vec::new();
        for s in &f.sections {
            let (from, to) = (index(&s.from)?, index(&s.to)?);
            ensure!(from != to, "section from {} to itself", s.from);
            for p in &s.paths {
                sections.push(Section { from, to, re: ci(p)? });
            }
        }
        let comments = match &f.comment_sections {
            None => None,
            Some(c) => {
                let from = c.from.iter().map(|d| index(d)).collect::<Result<Vec<_>>>()?;
                let to = index(&c.to)?;
                ensure!(!from.contains(&to), "comment sections move {} to itself", c.to);
                Some(Comments {
                    from,
                    to,
                    window: c.window,
                    inline_chars: c.inline_chars,
                    min_meta_before: c.min_meta_before,
                    timestamp_max_words: c.timestamp_max_words,
                    headers: c.headers.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                    meta: c.meta.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                    timestamp: ci(&c.timestamp)?,
                    not_meta: c.not_meta.iter().map(|p| ci(p)).collect::<Result<_>>()?,
                })
            }
        };
        Ok(Rules {
            version: f.version,
            sha256: grammar_a_pools::text::sha_hex(text.as_bytes()),
            min_cues: f.min_cues,
            content_before_weak: f.content_before_weak,
            domains,
            cue_res,
            multi_author: f.multi_author_hosts.iter().map(|h| h.to_lowercase()).collect(),
            sections,
            comments,
        })
    }

    /// The default pilot rules (v3).
    pub fn pilot() -> Rules {
        Rules::parse(PILOT_RULES).expect("pilot rules")
    }

    /// Rules v2 (the precision-v2 frame).
    pub fn pilot_v2() -> Rules {
        Rules::parse(PILOT_RULES_V2).expect("pilot rules v2")
    }

    /// The rules the pilot-v1 pool was built with.
    pub fn pilot_v1() -> Rules {
        Rules::parse(PILOT_RULES_V1).expect("pilot rules v1")
    }

    pub fn names(&self) -> Vec<&str> {
        self.domains.iter().map(|d| d.name.as_str()).collect()
    }

    /// What a rule id names in this rule file: `(kind, source)`, where the
    /// source is the host (with any path prefix) for host rules, the pattern
    /// as written for pattern and path rules, and the cue count for content
    /// cues. `None` for `general_web:no_rule` or an id this file does not
    /// hold. Rule indices are per file, so read an id with the rules that
    /// produced it.
    pub fn rule_source(&self, rule: &str) -> Option<(&'static str, String)> {
        let mut it = rule.splitn(3, ':');
        let (name, kind, key) = (it.next()?, it.next()?, it.next()?);
        let d = self.domains.iter().find(|d| d.name == name)?;
        let pat = |v: &[Regex]| -> Option<String> {
            let r = v.get(key.parse::<usize>().ok()?)?;
            Some(r.as_str().strip_prefix("(?i-u)").unwrap_or(r.as_str()).to_owned())
        };
        let host = |v: &[(String, Option<String>)]| -> Option<String> {
            v.iter()
                .map(|(h, pre)| format!("{h}{}", pre.as_deref().unwrap_or("")))
                .find(|h| h == key)
        };
        match kind {
            "host" => Some(("host", host(&d.hosts)?)),
            "weak_host" => Some(("weak_host", host(&d.weak_hosts)?)),
            "host_pattern" => Some(("host_pattern", pat(&d.host_re)?)),
            "weak_host_pattern" => Some(("weak_host_pattern", pat(&d.weak_host_re)?)),
            "path" => Some(("path", pat(&d.paths)?)),
            "weak_path" => Some(("weak_path", pat(&d.weak)?)),
            "content_cues" => Some(("content_cues", key.to_owned())),
            "section" => {
                let s = self.sections.get(key.parse::<usize>().ok()?)?;
                let r = s.re.as_str();
                (self.domains[s.to].name == name).then(|| ("section", r.strip_prefix("(?i-u)").unwrap_or(r).to_owned()))
            }
            "comment_section" => {
                let c = self.comments.as_ref()?;
                (self.domains[c.to].name == name && c.from.iter().any(|&i| self.domains[i].name == key))
                    .then(|| ("comment_section", key.to_owned()))
            }
            _ => None,
        }
    }

    /// Tier-1 domain from the URL alone, if a tier-1 rule fires.
    pub fn strong_url_rule(&self, host: &str, path: &str) -> Option<(usize, String)> {
        let path_b = path.as_bytes();
        let path_lc = path.to_lowercase();
        for (i, d) in self.domains.iter().enumerate() {
            if let Some((h, pre)) = host_hit(&d.hosts, host, &path_lc) {
                return Some((i, format!("{}:host:{h}{}", d.name, pre.as_deref().unwrap_or(""))));
            }
            for (k, r) in d.host_re.iter().enumerate() {
                if r.is_match(host.as_bytes()) {
                    return Some((i, format!("{}:host_pattern:{k}", d.name)));
                }
            }
            for (k, r) in d.paths.iter().enumerate() {
                if r.is_match(path_b) {
                    return Some((i, format!("{}:path:{k}", d.name)));
                }
            }
        }
        None
    }

    /// Tier-2 rule: weak hosts, weak host patterns, then weak paths of each
    /// domain in priority order, each gated by the domain's cue count.
    fn weak_url_rule(&self, host: &str, path: &str, page: &[usize], excerpt: &[usize]) -> Option<(usize, String)> {
        let path_b = path.as_bytes();
        let path_lc = path.to_lowercase();
        for (i, d) in self.domains.iter().enumerate() {
            if page[i] < d.weak_min_cues || excerpt[i] < d.weak_min_excerpt_cues {
                continue;
            }
            if let Some((h, pre)) = host_hit(&d.weak_hosts, host, &path_lc) {
                return Some((i, format!("{}:weak_host:{h}{}", d.name, pre.as_deref().unwrap_or(""))));
            }
            for (k, r) in d.weak_host_re.iter().enumerate() {
                if r.is_match(host.as_bytes()) {
                    return Some((i, format!("{}:weak_host_pattern:{k}", d.name)));
                }
            }
            for (k, r) in d.weak.iter().enumerate() {
                if r.is_match(path_b) {
                    return Some((i, format!("{}:weak_path:{k}", d.name)));
                }
            }
        }
        None
    }

    /// Domain from the URL alone (tier 1, then ungated tier 2): the v1
    /// view, kept for diagnostics.
    pub fn url_rule(&self, host: &str, path: &str) -> Option<(usize, String)> {
        self.strong_url_rule(host, path).or_else(|| {
            let all = vec![usize::MAX; self.domains.len()];
            self.weak_url_rule(host, path, &all, &all)
        })
    }

    /// Distinct cues per domain, and whether any anchor fired.
    fn cue_counts(&self, text: &str) -> (Vec<usize>, Vec<bool>) {
        let mut n = vec![0usize; self.domains.len()];
        let mut anchored = vec![false; self.domains.len()];
        let b = text.as_bytes();
        for (d, anchor, r) in &self.cue_res {
            if r.is_match(b) {
                n[*d] += 1;
                anchored[*d] |= *anchor;
            }
        }
        (n, anchored)
    }

    pub fn cues(&self, text: &str) -> BTreeMap<String, usize> {
        let (n, _) = self.cue_counts(text);
        self.named(&n)
    }

    fn named(&self, n: &[usize]) -> BTreeMap<String, usize> {
        n.iter()
            .enumerate()
            .filter(|(_, k)| **k > 0)
            .map(|(d, k)| (self.domains[d].name.clone(), *k))
            .collect()
    }

    fn content_rule(&self, n: &[usize], ex: &[usize], ex_anchored: &[bool]) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for (i, d) in self.domains.iter().enumerate() {
            let k = n[i];
            let gated = ex[i] >= d.min_excerpt_cues && (!d.has_anchors || ex_anchored[i]);
            if d.content_assign && k >= d.min_cues && gated && best.is_none_or(|(_, b)| k > b) {
                best = Some((i, k));
            }
        }
        best
    }

    /// Page-only assignment (pass 1, before an excerpt exists): the page
    /// stands in for the excerpt.
    pub fn assign(&self, host: &str, path: &str, page: &str) -> Assignment {
        self.assign_with_excerpt(host, path, page, page)
    }

    /// Assignment with the chosen excerpt: content cues are counted on the
    /// page and the excerpt gates them; then the section and comment-section
    /// steps (rules v3) run on the result.
    pub fn assign_with_excerpt(&self, host: &str, path: &str, page: &str, excerpt: &str) -> Assignment {
        let mut a = self.base_assignment(host, path, page, excerpt);
        if let Some(s) = self.sections.iter().enumerate().find_map(|(k, s)| {
            (self.domains[s.from].name == a.domain && s.re.is_match(path.as_bytes())).then_some((k, s))
        }) {
            a.domain = self.domains[s.1.to].name.clone();
            a.rule = format!("{}:section:{}", a.domain, s.0);
        }
        if let Some(c) = &self.comments
            && !std::ptr::eq(page, excerpt)
            && let Some(from) = c.from.iter().copied().find(|&i| self.domains[i].name == a.domain)
            && c.holds(page, excerpt) == Some(true)
        {
            a.domain = self.domains[c.to].name.clone();
            a.rule = format!("{}:comment_section:{}", a.domain, self.domains[from].name);
        }
        a
    }

    fn base_assignment(&self, host: &str, path: &str, page: &str, excerpt: &str) -> Assignment {
        let (n, _) = self.cue_counts(page);
        let (ex, ex_anchored) = if std::ptr::eq(page, excerpt) {
            self.cue_counts(page)
        } else {
            self.cue_counts(excerpt)
        };
        let done = |i: usize, rule: String| Assignment {
            domain: self.domains[i].name.clone(),
            rule,
            cues: self.named(&n),
            excerpt_cues: self.named(&ex),
        };
        if let Some((i, rule)) = self.strong_url_rule(host, path) {
            return done(i, rule);
        }
        let content = self.content_rule(&n, &ex, &ex_anchored);
        if self.content_before_weak
            && let Some((i, k)) = content
        {
            return done(i, format!("{}:content_cues:{k}", self.domains[i].name));
        }
        if let Some((i, rule)) = self.weak_url_rule(host, path, &n, &ex) {
            return done(i, rule);
        }
        match content {
            Some((i, k)) => done(i, format!("{}:content_cues:{k}", self.domains[i].name)),
            None => Assignment {
                domain: GENERAL.to_owned(),
                rule: "general_web:no_rule".to_owned(),
                cues: self.named(&n),
                excerpt_cues: self.named(&ex),
            },
        }
    }

    /// Group key: the registered domain, except on multi-author platforms,
    /// where a user subdomain is its own group and otherwise the first path
    /// segment (author, story or thread prefix) joins the host.
    pub fn group(&self, host: &str, domain: &str, path: &str) -> (String, &'static str) {
        if self.multi_author.iter().any(|m| m == domain) {
            let h = host.strip_prefix("www.").unwrap_or(host);
            if h != domain {
                return (format!("cc_host:{h}"), "multi_author_subdomain");
            }
            let seg = path.trim_start_matches('/').split(['/', '?', '#']).next().unwrap_or("");
            return (format!("cc_host:{h}/{}", seg.to_lowercase()), "multi_author_first_path_segment");
        }
        (format!("cc_site:{domain}"), "registered_domain")
    }
}

/// Path (with query) of an absolute URL, `/` when empty.
pub fn url_path(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    match rest.find(['/', '?', '#']) {
        Some(i) => &rest[i..],
        None => "/",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_url_rules_cues_and_groups() {
        let r = Rules::pilot_v1();
        assert_eq!(r.names().len(), 9);
        let a = r.assign("transcripts.cnn.com", "/TRANSCRIPTS/0101/01/sotu.html", "");
        assert_eq!(a.domain, "speech");
        let a = r.assign("www.cnn.com", "/2019/05/02/politics/some-long-story-slug/index.html", "");
        assert_eq!(a.domain, "news");
        let a = r.assign("someone.wordpress.com", "/2015/03/04/a-post-about-things/", "");
        assert_eq!(a.domain, "essays");
        let a = r.assign("www.example.com", "/viewtopic.php?t=12", "");
        assert_eq!(a.domain, "forum");
        let a = r.assign(
            "www.example.com",
            "/x",
            "The plaintiff argued, pursuant to the contract, that the defendant failed.",
        );
        let b = r.assign("www.example.com", "/x", "SMITH: Thank you. Click Posted: here.\nTags: none");
        assert_eq!(b.domain, GENERAL, "{b:?}");
        assert_eq!(a.domain, "legal");
        assert!(a.rule.starts_with("legal:content_cues:"));
        let a = r.assign("www.example.com", "/x", "Nothing to see.");
        assert_eq!(a.domain, GENERAL);
        // v1 ignores the excerpt: cues come from the page.
        let p = "The plaintiff argued, pursuant to the contract, that the defendant failed.";
        assert_eq!(r.assign_with_excerpt("www.example.com", "/x", p, "Nothing.").domain, "legal");
        // v1 weak paths fire without cues, before content cues.
        assert_eq!(
            r.assign("blog.example.org", "/2014/02/25/a-gadget-hands-on-preview/", "").domain,
            "essays"
        );
        assert_eq!(r.assign("www.example.org", "/2014/02/25/a-gadget-hands-on-preview/", "").domain, "news");
        assert_eq!(
            r.group("me.blogspot.com", "me.blogspot.com", "/2015/01/x.html").0,
            "cc_site:me.blogspot.com"
        );
        assert_eq!(r.group("medium.com", "medium.com", "/@writer/post-1").0, "cc_host:medium.com/@writer");
        assert_eq!(url_path("https://a.example.com/b/c?d=1"), "/b/c?d=1");
        assert_eq!(url_path("https://a.example.com"), "/");
        assert!(!host_matches("notnytimes.com", "nytimes.com"));
        assert!(host_matches("www.nytimes.com", "nytimes.com"));
    }

    /// The v2 fixes from the precision review, on synthetic URLs and text.
    #[test]
    fn v2_precision_review_fixes() {
        let r = Rules::pilot_v2();
        assert_eq!(r.version, "cc_human_domain_rules_pilot_v2");
        assert_eq!(r.names().len(), 9);
        let d = |host: &str, path: &str, ex: &str| r.assign_with_excerpt(host, path, ex, ex).domain;
        let plain = "The weather was mild and the market stayed open until late in the evening.";
        // Day-dated slugs are blog permalinks: essays with first-person cues,
        // news only with news anchors, otherwise general_web.
        let slug = "/2016/04/12/a-long-post-about-the-weekend/";
        assert_eq!(d("www.example.org", slug, "I went to the lake and I was happy."), "essays");
        assert_eq!(d("www.example.org", slug, "Two people were hurt, police said on Monday."), "news");
        assert_eq!(d("www.example.org", slug, plain), GENERAL);
        // News hosts still decide alone.
        assert_eq!(d("www.cnn.com", "/2019/05/02/politics/some-long-story-slug/index.html", plain), "news");
        // How-to slugs, FAQs and guides need technical cues.
        assert_eq!(d("www.example.com", "/how-to-make-caramel-popcorn/", plain), GENERAL);
        let tech = "Open the configuration file and run the following command on Linux, then restart the server.";
        assert_eq!(d("www.example.com", "/how-to-fix-the-boot-error/", tech), "technical");
        assert_eq!(d("www.example.com", "/faq/", plain), GENERAL);
        assert_eq!(d("www.example.com", "/how-to/", plain), "technical");
        // Technology words in page navigation are not technical content.
        let nav = "Menu: Android | Windows | Plugin | API | Install | Settings | Software";
        assert_eq!(r.assign_with_excerpt("www.example.com", "/x", nav, plain).domain, GENERAL);
        // Speech paths are whole segments; debates and interviews need speaker cues.
        assert_eq!(d("www.example.com", "/report/speech-recognition-market-forecast", plain), GENERAL);
        assert_eq!(d("www.example.com", "/community/debate-over-the-new-mall/", plain), GENERAL);
        assert_eq!(d("www.example.com", "/news/remarks-by-the-mayor-at-the-opening", plain), "speech");
        assert_eq!(d("www.example.com", "/interviews/a-band", plain), GENERAL);
        assert_eq!(
            d(
                "www.example.com",
                "/interviews/a-band",
                "HOST: Welcome back.\nGUEST: Thanks for having me."
            ),
            "speech"
        );
        // FAQ lines are not speaker turns.
        let faq = "Q : Can I return it?\nA : Yes, within 30 days.\nQ: Shipping?\nA: Free. Read the transcript of our policy.";
        assert_eq!(d("shop.example.com", "/item-1", faq), GENERAL);
        // Chapter paths need fiction cues; story hosts are narrowed to story pages.
        assert_eq!(d("laws.example.gov", "/title-4/chapter-12/section-3", plain), GENERAL);
        let story = "\"Wait,\" she whispered. He nodded and stepped back, and his eyes followed her to the door.";
        assert_eq!(d("www.example.com", "/my-story/chapter-12", story), "fiction");
        assert_eq!(d("www.fanfiction.net", "/s/123/4/A-Title", plain), "fiction");
        assert_eq!(d("www.fictionpress.com", "/r/123/0/7/", plain), GENERAL);
        assert_eq!(d("intranet.example.edu", "/s/751/2013/index.aspx", plain), GENERAL);
        // The bare mail.* host is not a list archive; forum hosts win over /msgN.html.
        assert_eq!(d("mail.example.org", "/recipes/soup.html", plain), GENERAL);
        assert_eq!(d("forums.example.com", "/load/garden/msg0512.html", plain), "forum");
        assert_eq!(d("lists.example.org", "/pipermail/dev/2009/000123.html", plain), "email");
        // Legal content needs a legal anchor, not only court vocabulary.
        let court_news = "The supreme court heard the statute challenge; jurisdiction and liability were argued.";
        assert_eq!(d("www.example.com", "/x", court_news), GENERAL);
        let filing = "The plaintiff argued, pursuant to the statute, that the defendant lacked jurisdiction.";
        assert_eq!(d("www.example.com", "/x", filing), "legal");
        // Hosts that merely end in forum.org need a forum cue.
        assert_eq!(d("www.researchforum.org", "/news/new-lessons", plain), GENERAL);
        // Retailer hosts are not review hosts; a review there is found by its cues.
        assert_eq!(
            d("www.walmart.com", "/ip/a-rug/1234", "A braided rug in warm red shades for any room."),
            GENERAL
        );
        assert_eq!(
            d("www.walmart.com", "/ip/a-rug/1234", "I bought this rug in May and I would recommend it."),
            "reviews"
        );
        // Generic review sections need evaluative words in the excerpt.
        assert_eq!(d("www.example.com", "/reviews/main/coffee/glossary.asp", plain), GENERAL);
        assert_eq!(
            d(
                "www.example.com",
                "/reviews/an-album/",
                "The album opens well and the vocals soar; I loved it."
            ),
            "reviews"
        );
        // News section paths alone are not news.
        assert_eq!(d("www.example.org", "/articles/how-rates-affect-loans", plain), GENERAL);
        // Review cues on product pages: the excerpt must carry them.
        let a = r.assign_with_excerpt("shop.example.com", "/item", "Verified purchase. 5 out of 5 stars.", plain);
        assert_eq!(a.domain, GENERAL);
        // Blog subdomains are gated by first-person cues.
        assert_eq!(
            d("blog.example.com", "/win-this-tray", "Enter today to win this tray from the shop."),
            GENERAL
        );
        assert_eq!(d("blog.example.com", "/our-trip", "We went north and I was glad we did."), "essays");
        assert_eq!(d("someone.blogspot.co.at", "/2013/06/", plain), "essays");
    }

    /// The v3 changes from the fresh v2 precision draw, on synthetic URLs and
    /// text: no content-cue entry into technical or reviews, newspaper
    /// opinion sections to essays, blog and news comment sections to forum.
    #[test]
    fn v3_fresh_draw_fixes() {
        let r = Rules::pilot();
        let v2 = Rules::pilot_v2();
        assert_eq!(r.version, "cc_human_domain_rules_pilot_v3");
        assert_eq!(r.names(), v2.names());
        let d = |r: &Rules, host: &str, path: &str, ex: &str| r.assign_with_excerpt(host, path, ex, ex).domain;
        let tech = "Open the configuration file and run the following command on Linux, then restart the server. \
                    Install the driver, check the API settings and compile the plugin before you reboot.";
        // Content cues alone no longer make a page technical; URL rules still do.
        assert_eq!(d(&v2, "www.example.com", "/x", tech), "technical");
        assert_eq!(d(&r, "www.example.com", "/x", tech), GENERAL);
        assert_eq!(d(&r, "www.example.com", "/how-to-fix-the-boot-error/", tech), "technical");
        // The same for reviews: a retailer page with review words stays general_web.
        let review = "I bought this rug in May and I would recommend it.";
        assert_eq!(d(&v2, "www.walmart.com", "/ip/a-rug/1234", review), "reviews");
        assert_eq!(d(&r, "www.walmart.com", "/ip/a-rug/1234", review), GENERAL);
        let a = r.assign_with_excerpt("www.example.com", "/reviews/an-album/", "The album opens well and the vocals soar; I loved it.", "The album opens well and the vocals soar; I loved it.");
        assert_eq!((a.domain.as_str(), r.rule_source(&a.rule).map(|x| x.0)), ("reviews", Some("weak_path")));
        // Newspaper opinion and column sections go to essays; news sections stay news.
        let plain = "The weather was mild and the market stayed open until late in the evening.";
        assert_eq!(d(&v2, "www.cnn.com", "/opinion/2019/05/02/a-view/index.html", plain), "news");
        let op = r.assign_with_excerpt("www.cnn.com", "/opinion/2019/05/02/a-view/index.html", plain, plain);
        assert_eq!(op.domain, "essays");
        assert_eq!(r.rule_source(&op.rule).map(|x| x.0), Some("section"));
        for path in ["/comment/a-column", "/columnists/someone/a-piece", "/news/letters/a-letter", "/editorials/1234"] {
            assert_eq!(d(&r, "www.cnn.com", path, plain), "essays", "{path}");
        }
        for path in ["/2019/05/02/politics/some-long-story-slug/index.html", "/a-story/comment-page-2/", "/editorial/1234", "/opinionated"] {
            assert_eq!(d(&r, "www.cnn.com", path, plain), "news", "{path}");
        }
        // Sections apply only to documents of their `from` domain.
        assert_eq!(d(&r, "someone.blogspot.com", "/opinion/2015/03/", "I went to the lake and I was happy."), "essays");
        // (Essays' own opinion paths already decide off news hosts; letters pages are general_web there.)
        assert_eq!(d(&r, "www.example.com", "/opinion/x", plain), "essays");
        assert_eq!(d(&r, "www.example.com", "/letters/x", plain), GENERAL);
        assert_eq!(d(&r, "www.cnn.com", "/letters/x", plain), "essays");
        // Comment sections: the excerpt after a comment header and comment meta lines is forum.
        let post = "I went to the lake on Sunday and I was happy that I did, because the water was calm and warm.";
        let comment = "Great post, I agree with you and I think the lake is lovely in the spring as well.";
        let page = format!(
            "My weekend\n{post}\nPosted by Jane at 9:41 PM\n2 comments:\nSam said...\nI was there too and it was lovely, thanks.\nMarch 4, 2014 at 3:23 PM\nAlex said...\n{comment}\nMarch 5, 2014 at 9:02 AM\nPost a Comment"
        );
        let c = r.assign_with_excerpt("someone.blogspot.com", "/2014/03/my-weekend.html", &page, comment);
        assert_eq!(c.domain, "forum");
        assert_eq!(c.rule, "forum:comment_section:essays");
        assert_eq!(r.rule_source(&c.rule), Some(("comment_section", "essays".to_owned())));
        // The post itself stays essays, and v2 never reads the comment section.
        assert_eq!(r.assign_with_excerpt("someone.blogspot.com", "/2014/03/my-weekend.html", &page, post).domain, "essays");
        assert_eq!(v2.assign_with_excerpt("someone.blogspot.com", "/2014/03/my-weekend.html", &page, comment).domain, "essays");
        // A comment timestamp at the start of the excerpt counts as its meta line.
        let inline = format!("Robin on March 4, 2014 at 3:23 pm {comment}");
        let page2 = format!("A story\n{post}\n3 Responses to the story\nKim on March 4, 2014 at 1:02 pm I liked this story a lot, thank you for it.\n{inline}");
        assert_eq!(r.assign_with_excerpt("www.cnn.com", "/2014/03/04/a-story/", &page2, &inline).domain, "forum");
        // Post metadata alone (a dated byline, an add-a-comment link) is not a comment section.
        let page3 = format!("A story\nPosted on March 4, 2014 at 3:23 pm by Jane\nAdd Comment\n{post}\nLeave a Reply");
        assert_eq!(r.assign_with_excerpt("someone.blogspot.com", "/2014/03/a-story.html", &page3, post).domain, "essays");
        // An excerpt not found on the page is left alone, and pass 1 (page only) never moves.
        assert_eq!(r.assign_with_excerpt("someone.blogspot.com", "/2014/03/my-weekend.html", &page, "Not on the page at all.").domain, "essays");
        assert_eq!(r.assign("someone.blogspot.com", "/2014/03/my-weekend.html", &page).domain, "essays");
        // Forum's comment rule ids resolve only for listed `from` domains.
        assert_eq!(r.rule_source("forum:comment_section:legal"), None);
        assert_eq!(r.rule_source("essays:section:0").map(|x| x.0), Some("section"));
        assert_eq!(r.rule_source("news:section:0"), None);
        assert_eq!(d(&r, "healthunlocked.com", "/couch/posts/1/a-run", plain), "forum");
    }

    #[test]
    fn engine_sections_comments_and_content_assign() {
        let r = Rules::parse(
            r#"{"version": "t", "priority": ["forum", "news", "essays"], "min_cues": 1,
                "domains": {
                  "forum": {"hosts": [], "host_patterns": [], "paths": [], "weak_paths": [], "cues": ["\\bthread\\b"]},
                  "news": {"hosts": ["paper.example"], "host_patterns": [], "paths": [], "weak_paths": [], "content_assign": false,
                           "cues": ["\\bofficials said\\b"]},
                  "essays": {"hosts": ["blog.example"], "host_patterns": [], "paths": [], "weak_paths": [], "cues": ["\\bi think\\b"]}},
                "multi_author_hosts": [],
                "sections": [{"from": "news", "to": "essays", "paths": ["/opinion/"]}],
                "comment_sections": {"from": ["essays"], "to": "forum", "window": 1, "inline_chars": 10, "min_meta_before": 2,
                  "timestamp_max_words": 4, "headers": ["^[0-9]+ comments$"], "meta": ["said:$"], "timestamp": "[0-9]{1,2}:[0-9]{2}",
                  "not_meta": ["^posted"]}}"#,
        )
        .unwrap();
        let same = |host: &str, path: &str, t: &str| r.assign_with_excerpt(host, path, t, t);
        // content_assign false: news cues alone do not assign news, but its hosts do.
        assert_eq!(same("x.example", "/a", "Officials said so.").domain, GENERAL);
        assert_eq!(same("x.example", "/a", "In this thread I think.").domain, "forum");
        assert_eq!(same("paper.example", "/a", "Plain.").domain, "news");
        let s = same("paper.example", "/opinion/a", "Plain.");
        assert_eq!((s.domain.as_str(), s.rule.as_str()), ("essays", "essays:section:0"));
        assert_eq!(r.rule_source(&s.rule), Some(("section", "/opinion/".to_owned())));
        // Comment sections: header plus a meta line in the window.
        let page = "Post text here.\n2 comments\nAnn said:\nThe comment text.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The comment text.").domain, "forum");
        // Two meta lines stand in for a header; one alone does not.
        let page = "Post.\nBo said:\nFirst.\nAnn said:\nThe comment text.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The comment text.").domain, "forum");
        let page = "Post.\nAnn said:\nThe comment text.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The comment text.").domain, "essays");
        // The meta line must be inside the window; a short timestamp line counts, a posted-by line does not.
        let page = "2 comments\nAnn said:\nOther.\nThe comment text.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The comment text.").domain, "essays");
        let page = "2 comments\nat 9:41\nThe comment text.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The comment text.").domain, "forum");
        let page = "2 comments\nposted at 9:41\nThe comment text.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The comment text.").domain, "essays");
        // Multi-paragraph excerpts are located line by line; squashed spacing matches.
        let page = "2 comments\nAnn said:\n  The   first.  \nThe second.";
        assert_eq!(r.assign_with_excerpt("blog.example", "/p", page, "The first.\n\nThe second.").domain, "forum");
    }

    #[test]
    fn engine_gates_weak_rules_and_requires_anchors() {
        let r = Rules::parse(
            r#"{"version": "t", "priority": ["news", "essays"], "min_cues": 2,
                "weak_min_cues": 0, "weak_min_excerpt_cues": 1, "content_before_weak": true,
                "domains": {
                  "news": {"hosts": [], "host_patterns": [], "paths": [], "weak_paths": ["/news/"],
                           "weak_hosts": ["paper.example/archive"], "min_excerpt_cues": 1,
                           "cues": ["\\bofficials said\\b", "\\bon monday\\b"], "anchors": ["\\(AP\\)"]},
                  "essays": {"hosts": ["blog.example"], "host_patterns": [], "paths": [], "weak_paths": [],
                             "weak_min_cues": 1, "weak_min_excerpt_cues": 0, "weak_host_patterns": ["^diary\\."],
                             "cues": ["\\bi think\\b", "\\bi believe\\b"]}},
                "multi_author_hosts": []}"#,
        )
        .unwrap();
        let same = |host: &str, path: &str, t: &str| r.assign_with_excerpt(host, path, t, t);
        // Tier 1 decides whatever the text says.
        assert_eq!(same("me.blog.example", "/x", "Officials said (AP)").domain, "essays");
        // A gated weak path needs its cue in the excerpt; page cues do not count.
        let none = r.assign_with_excerpt("x.example", "/news/item", "officials said on monday", "Plain text.");
        assert_eq!(none.domain, GENERAL);
        let one = r.assign_with_excerpt("x.example", "/news/item", "", "Officials said nothing.");
        assert_eq!((one.domain.as_str(), one.rule.as_str()), ("news", "news:weak_path:0"));
        // A page-gated weak host pattern reads the page instead.
        assert_eq!(
            r.assign_with_excerpt("diary.example", "/a", "I think so.", "Plain text.").domain,
            "essays"
        );
        assert_eq!(r.assign_with_excerpt("diary.example", "/a", "Plain page.", "I think so.").domain, GENERAL);
        // Weak hosts with a path prefix are gated the same way.
        assert_eq!(same("paper.example", "/archive/1", "On Monday it rained.").domain, "news");
        assert_eq!(same("paper.example", "/other/1", "On Monday it rained.").domain, GENERAL);
        // Content cues count on the page, need the anchor in the excerpt...
        assert_eq!(same("x.example", "/y", "Officials said so on Monday.").domain, GENERAL);
        let anchored = same("x.example", "/y", "WASHINGTON (AP) officials said so on Monday.");
        assert_eq!(anchored.rule, "news:content_cues:3");
        let page_only = r.assign_with_excerpt("x.example", "/y", "WASHINGTON (AP) officials said so on Monday.", "Plain.");
        assert_eq!(page_only.domain, GENERAL);
        assert_eq!(page_only.cues.get("news"), Some(&3));
        assert!(page_only.excerpt_cues.is_empty());
        // ...and no anchor when the domain lists none.
        assert_eq!(same("x.example", "/y", "I think and I believe.").domain, "essays");
        // Content cues decide before a weak rule of another domain.
        let c = same("x.example", "/news/z", "I think so and I believe it; officials said.");
        assert_eq!((c.domain.as_str(), c.rule.as_str()), ("essays", "essays:content_cues:2"));
        // Rule ids resolve to what they name in this file.
        assert_eq!(r.rule_source(&one.rule), Some(("weak_path", "/news/".to_owned())));
        assert_eq!(
            r.rule_source("news:weak_host:paper.example/archive"),
            Some(("weak_host", "paper.example/archive".to_owned()))
        );
        assert_eq!(r.rule_source("essays:weak_host_pattern:0"), Some(("weak_host_pattern", "^diary\\.".to_owned())));
        assert_eq!(r.rule_source("essays:host:blog.example"), Some(("host", "blog.example".to_owned())));
        assert_eq!(r.rule_source("essays:content_cues:2"), Some(("content_cues", "2".to_owned())));
        assert_eq!(r.rule_source("news:weak_path:7"), None);
        assert_eq!(r.rule_source("essays:host:other.example"), None);
        assert_eq!(r.rule_source("general_web:no_rule"), None);
    }
}

//! robots.txt under RFC 9309 (`robots_rfc9309_v1`): the parser that keeps
//! the group applying to SlopNinjaBot, and the matcher the `DetectHost`
//! object and the fetch Worker evaluate per path.
//!
//! - **Groups (2.1, 2.2.1).** One or more `user-agent` lines open a group;
//!   its rules run to the next `user-agent` line that follows a rule. The
//!   product token is matched case-insensitively against each line's
//!   leading token (`[A-Za-z_-]+`). Every group naming SlopNinjaBot is
//!   combined; without one, every `*` group is; without either, nothing
//!   applies and everything is allowed. Lines outside any group, sitemaps
//!   and unknown keys are ignored.
//! - **Rules (2.2.2).** `allow` and `disallow` (and the common misspellings
//!   `dissallow` and `disalow`) with a non-empty path pattern. `*` matches
//!   any run of octets and a final `$` anchors the end. The most specific
//!   match (most octets) wins, and on a tie `allow` wins. `/robots.txt` is
//!   always allowed (2.2.2). Patterns and paths are compared after the same
//!   percent-encoding normalization (2.2.2): unreserved octets are decoded,
//!   other escapes upper-cased, and octets outside ASCII or controls encoded.
//! - **Crawl-delay** (a common extension) from the matched groups, in ms.
//! - **Content signals** (`Content-Signal: search=yes, ai-input=no, ...`)
//!   from the matched groups: `ai-input=no` means no fetch (like a
//!   disallow), `search=no` means no public page (risk F8).
//! - **Size (2.5).** Only the first [`ROBOTS_PARSE_BYTES`] are parsed (cut at
//!   the last line end inside them).
//!
//! Matching is the two-pointer wildcard algorithm, O(pattern × path) with no
//! backtracking blow-up.

use serde::{Deserialize, Serialize};

/// The rules' version.
pub const ROBOTS_V: &str = "robots_rfc9309_v1";
/// The bytes of robots.txt parsed: RFC 9309 section 2.5's minimum.
pub const ROBOTS_PARSE_BYTES: usize = 500 * 1024;
/// How long rules may be cached: RFC 9309 section 2.4's limit.
pub const ROBOTS_CACHE_MS: u64 = 24 * 60 * 60 * 1000;

/// Which groups applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMatch {
    /// Groups naming the product token.
    Product,
    /// The `*` groups.
    Any,
    /// No group applies: everything is allowed.
    #[default]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub allow: bool,
    /// The normalized pattern.
    pub pattern: String,
}

/// Content signals of the matched groups (`None`: not stated).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signals {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_input: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_train: Option<bool>,
}

/// The rules that apply to SlopNinjaBot on one origin.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RobotsRules {
    pub v: String,
    pub group: GroupMatch,
    pub rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crawl_delay_ms: Option<u64>,
    #[serde(default)]
    pub signals: Signals,
}

/// What the rules decide for one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// A disallow rule matched.
    Disallowed,
    /// `ai-input=no`: no fetch.
    NoAiInput,
    /// `search=no`: no public page.
    NoSearch,
}

impl Decision {
    /// The refusal code the pages print.
    pub fn refusal(self) -> Option<&'static str> {
        match self {
            Decision::Allowed => None,
            Decision::Disallowed | Decision::NoAiInput => Some("robots"),
            Decision::NoSearch => Some("private_source"),
        }
    }
}

impl RobotsRules {
    /// Everything allowed (a 4xx robots.txt).
    pub fn allow_all() -> RobotsRules {
        RobotsRules { v: ROBOTS_V.to_owned(), ..RobotsRules::default() }
    }

    /// Everything disallowed (an unreachable robots.txt).
    pub fn disallow_all() -> RobotsRules {
        RobotsRules {
            v: ROBOTS_V.to_owned(),
            group: GroupMatch::Any,
            rules: vec![Rule { allow: false, pattern: "/".to_owned() }],
            ..RobotsRules::default()
        }
    }

    /// Parses a robots.txt body for `product` (the crawler's product token).
    pub fn parse(body: &[u8], product: &str) -> RobotsRules {
        let body = cut(body);
        let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
        let product = product.to_ascii_lowercase();
        // (agents, rules, delay, signals) per group.
        let mut groups: Vec<Group> = Vec::new();
        let mut in_agents = false;
        for raw in body.split(|&b| b == b'\n' || b == b'\r') {
            let line = match raw.iter().position(|&b| b == b'#') {
                Some(i) => raw.get(..i).unwrap_or(raw),
                None => raw,
            };
            let Some(colon) = line.iter().position(|&b| b == b':') else { continue };
            let key = trim(line.get(..colon).unwrap_or(&[])).to_ascii_lowercase();
            let value = trim(line.get(colon + 1..).unwrap_or(&[]));
            match key.as_slice() {
                b"user-agent" | b"useragent" | b"user agent" => {
                    if !in_agents || groups.is_empty() {
                        groups.push(Group::default());
                    }
                    in_agents = true;
                    if let Some(g) = groups.last_mut() {
                        g.agents.push(agent_token(value));
                    }
                }
                b"allow" | b"disallow" | b"dissallow" | b"disalow" => {
                    in_agents = false;
                    if value.is_empty() {
                        continue;
                    }
                    if let Some(g) = groups.last_mut() {
                        g.rules.push(Rule { allow: key.as_slice() == b"allow", pattern: normalize(value) });
                    }
                }
                b"crawl-delay" | b"crawldelay" => {
                    in_agents = false;
                    if let Some(g) = groups.last_mut()
                        && g.delay.is_none()
                    {
                        g.delay = delay_ms(value);
                    }
                }
                b"content-signal" => {
                    in_agents = false;
                    if let Some(g) = groups.last_mut() {
                        signals(value, &mut g.signals);
                    }
                }
                _ => {}
            }
        }
        // The groups that apply are moved out, never copied.
        let names = |want: &str| groups.iter().any(|g| g.agents.iter().any(|a| a == want));
        let (group, want) = if names(&product) {
            (GroupMatch::Product, product.as_str())
        } else if names("*") {
            (GroupMatch::Any, "*")
        } else {
            (GroupMatch::None, "\0")
        };
        let mut out = RobotsRules { v: ROBOTS_V.to_owned(), group, ..RobotsRules::default() };
        for g in groups.iter_mut().filter(|g| g.agents.iter().any(|a| a == want)) {
            if out.rules.is_empty() {
                out.rules = std::mem::take(&mut g.rules);
            } else {
                out.rules.append(&mut g.rules);
            }
            if out.crawl_delay_ms.is_none() {
                out.crawl_delay_ms = g.delay;
            }
            let s = &mut out.signals;
            s.search = s.search.or(g.signals.search);
            s.ai_input = s.ai_input.or(g.signals.ai_input);
            s.ai_train = s.ai_train.or(g.signals.ai_train);
        }
        out
    }

    /// Whether the rules allow `path` (path and query, as the URL parser
    /// serializes them).
    pub fn allows(&self, path: &str) -> bool {
        let path = normalize(path.as_bytes());
        if path == "/robots.txt" {
            return true;
        }
        let mut best: Option<(usize, bool)> = None;
        for r in &self.rules {
            if matches(&r.pattern, &path) {
                let len = r.pattern.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, r.allow)),
                };
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }

    /// The decision for `path`: a disallow first, then the content signals.
    pub fn decide(&self, path: &str) -> Decision {
        if !self.allows(path) {
            Decision::Disallowed
        } else if self.signals.ai_input == Some(false) {
            Decision::NoAiInput
        } else if self.signals.search == Some(false) {
            Decision::NoSearch
        } else {
            Decision::Allowed
        }
    }
}

#[derive(Default)]
struct Group {
    agents: Vec<String>,
    rules: Vec<Rule>,
    delay: Option<u64>,
    signals: Signals,
}

/// The first [`ROBOTS_PARSE_BYTES`], cut at the last line end inside them.
fn cut(body: &[u8]) -> &[u8] {
    if body.len() <= ROBOTS_PARSE_BYTES {
        return body;
    }
    let head = body.get(..ROBOTS_PARSE_BYTES).unwrap_or(body);
    match head.iter().rposition(|&b| b == b'\n') {
        Some(i) => head.get(..i).unwrap_or(head),
        None => head,
    }
}

fn trim(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(s.len());
    let end = s.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(start, |e| e + 1);
    s.get(start..end).unwrap_or(&[])
}

/// A user-agent line's product token, lowercased (`*` stays `*`).
fn agent_token(value: &[u8]) -> String {
    if value.first() == Some(&b'*') {
        return "*".to_owned();
    }
    value.iter().take_while(|b| b.is_ascii_alphabetic() || **b == b'_' || **b == b'-').map(|b| char::from(b.to_ascii_lowercase())).collect()
}

fn delay_ms(value: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(value).ok()?;
    let secs: f64 = s.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    // Saturating float-to-int conversion.
    Some((secs * 1000.0).round() as u64)
}

fn signals(value: &[u8], out: &mut Signals) {
    for pair in value.split(|&b| b == b',') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else { continue };
        let k = trim(pair.get(..eq).unwrap_or(&[])).to_ascii_lowercase();
        let v = trim(pair.get(eq + 1..).unwrap_or(&[])).to_ascii_lowercase();
        let yes = match v.as_slice() {
            b"yes" => true,
            b"no" => false,
            _ => continue,
        };
        let slot = match k.as_slice() {
            b"search" => &mut out.search,
            b"ai-input" => &mut out.ai_input,
            b"ai-train" => &mut out.ai_train,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(yes);
        }
    }
}

fn hex(n: u8) -> Option<u8> {
    match n {
        b'0'..=b'9' => Some(n - b'0'),
        b'a'..=b'f' => Some(n - b'a' + 10),
        b'A'..=b'F' => Some(n - b'A' + 10),
        _ => None,
    }
}

/// RFC 9309 section 2.2.2's comparison form: unreserved escapes decoded,
/// other escapes upper-cased, non-ASCII, controls and spaces encoded.
fn push_escape(out: &mut String, v: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    for nibble in [v >> 4, v & 15] {
        out.push(char::from(HEX.get(usize::from(nibble)).copied().unwrap_or(b'0')));
    }
}

pub fn normalize(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(&b) = s.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (s.get(i + 1).copied().and_then(hex), s.get(i + 2).copied().and_then(hex))
        {
            let v = h * 16 + l;
            if v.is_ascii_alphanumeric() || b"-._~".contains(&v) {
                out.push(char::from(v));
            } else {
                push_escape(&mut out, v);
            }
            i += 3;
            continue;
        }
        if b > 0x7e || b <= 0x20 {
            push_escape(&mut out, b);
        } else {
            out.push(char::from(b));
        }
        i += 1;
    }
    out
}

/// Whether `pattern` (`*` wildcards, optional final `$`) matches `path`
/// from its start.
pub fn matches(pattern: &str, path: &str) -> bool {
    let (p, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p.as_bytes(), true),
        None => (pattern.as_bytes(), false),
    };
    let s = path.as_bytes();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    loop {
        if pi == p.len() && (!anchored || si == s.len()) {
            return true;
        }
        match p.get(pi) {
            Some(b'*') => {
                star = Some((pi, si));
                pi += 1;
                continue;
            }
            Some(&c) if s.get(si) == Some(&c) => {
                pi += 1;
                si += 1;
                continue;
            }
            _ => {}
        }
        // Backtrack: let the last `*` absorb one more octet.
        match star {
            Some((sp, ss)) if ss < s.len() => {
                star = Some((sp, ss + 1));
                pi = sp + 1;
                si = ss + 1;
            }
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(txt: &str) -> RobotsRules {
        RobotsRules::parse(txt.as_bytes(), "SlopNinjaBot")
    }

    #[test]
    fn rfc9309_section_5_example() {
        // RFC 9309 section 5.1's example file and its expectations.
        let txt = "User-Agent: *\nDisallow: *.gif$\nDisallow: /example/\nAllow: /publications/\n\n\
                   User-Agent: foobot\nDisallow:/\nAllow:/example/page.html\nAllow:/example/allowed.gif\n\n\
                   User-Agent: barbot\nUser-Agent: bazbot\nDisallow: /example/page.html\n\n\
                   User-Agent: quxbot\n";
        let r = RobotsRules::parse(txt.as_bytes(), "foobot");
        assert_eq!(r.group, GroupMatch::Product);
        assert!(r.allows("/example/page.html"));
        assert!(r.allows("/example/allowed.gif"));
        assert!(!r.allows("/example/other.html"));
        assert!(!r.allows("/"));
        let b = RobotsRules::parse(txt.as_bytes(), "bazbot");
        assert!(!b.allows("/example/page.html"));
        assert!(b.allows("/example/other"));
        // quxbot's group is empty: nothing disallowed.
        let q = RobotsRules::parse(txt.as_bytes(), "quxbot");
        assert_eq!(q.group, GroupMatch::Product);
        assert!(q.allows("/example/page.html"));
        let other = RobotsRules::parse(txt.as_bytes(), "otherbot");
        assert_eq!(other.group, GroupMatch::Any);
        assert!(!other.allows("/a/b.gif"));
        assert!(other.allows("/a/b.gif?x"));
        assert!(!other.allows("/example/x"));
        assert!(other.allows("/publications/x"));
    }

    #[test]
    fn product_groups_combine_and_win_over_star() {
        let r = rules(
            "User-agent: *\nDisallow: /\n\nUser-agent: SlopNinjaBot/1.0\nDisallow: /private/\n\nuser-agent: slopninjabot\nDisallow: /tmp/\n",
        );
        assert_eq!(r.group, GroupMatch::Product);
        assert!(r.allows("/article"));
        assert!(!r.allows("/private/x"));
        assert!(!r.allows("/tmp/x"));
    }

    #[test]
    fn opt_out_by_name() {
        let r = rules("User-agent: SlopNinjaBot\nDisallow: /\n");
        assert_eq!(r.decide("/any/page"), Decision::Disallowed);
        assert!(r.allows("/robots.txt"));
    }

    #[test]
    fn longest_match_and_allow_on_ties() {
        let r =
            rules("User-agent: *\nAllow: /p\nDisallow: /p\nDisallow: /folder/\nAllow: /folder/page\nDisallow: /*.php$\nAllow: /x/*.php$\n");
        assert!(r.allows("/p"));
        assert!(r.allows("/folder/page"));
        assert!(!r.allows("/folder/other"));
        assert!(!r.allows("/a/b.php"));
        assert!(r.allows("/a/b.php5"));
        assert!(r.allows("/x/y.php"));
    }

    #[test]
    fn empty_rules_and_no_groups() {
        assert!(rules("User-agent: *\nDisallow:\n").allows("/anything"));
        assert_eq!(rules("").group, GroupMatch::None);
        assert!(rules("Disallow: /\n").allows("/x"), "a rule outside any group is ignored");
        assert!(rules("# only comments\nSitemap: https://example.com/s.xml\n").allows("/x"));
    }

    #[test]
    fn groups_end_at_the_next_agent_after_a_rule() {
        let r = rules("User-agent: other\nDisallow: /a\nSitemap: https://e.com/s\nUser-agent: SlopNinjaBot\nDisallow: /b\n");
        assert!(r.allows("/a"));
        assert!(!r.allows("/b"));
        // Consecutive agents share a group.
        let r = rules("User-agent: other\nUser-agent: SlopNinjaBot\nDisallow: /c\n");
        assert!(!r.allows("/c"));
    }

    #[test]
    fn percent_encoding_is_normalized() {
        let r = rules("User-agent: *\nDisallow: /foo/bar?baz=quz\nDisallow: /%7Ejoe/\nDisallow: /caf\u{e9}/\nDisallow: /a%2fb\n");
        assert!(!r.allows("/foo/bar?baz=quz"));
        assert!(!r.allows("/~joe/index"));
        assert!(!r.allows("/caf%C3%A9/menu"));
        assert!(!r.allows("/a%2Fb"));
        assert!(r.allows("/a/b"), "an encoded slash is reserved, not decoded");
    }

    #[test]
    fn crawl_delay_and_content_signals() {
        let r = rules("User-agent: *\nCrawl-delay: 2.5\nContent-Signal: search=yes, ai-input=no, ai-train=no\nAllow: /\n");
        assert_eq!(r.crawl_delay_ms, Some(2500));
        assert_eq!(r.signals, Signals { search: Some(true), ai_input: Some(false), ai_train: Some(false) });
        assert_eq!(r.decide("/x"), Decision::NoAiInput);
        let s = rules("User-agent: *\nContent-Signal: search=no\n");
        assert_eq!(s.decide("/x"), Decision::NoSearch);
        assert_eq!(Decision::NoSearch.refusal(), Some("private_source"));
        assert_eq!(rules("User-agent: *\nCrawl-delay: soon\n").crawl_delay_ms, None);
    }

    #[test]
    fn size_bound_and_odd_bytes() {
        let mut big = b"User-agent: *\nDisallow: /early\n".to_vec();
        while big.len() < ROBOTS_PARSE_BYTES {
            big.extend_from_slice(b"# padding padding padding padding padding padding\n");
        }
        big.extend_from_slice(b"Disallow: /late\n");
        let r = RobotsRules::parse(&big, "SlopNinjaBot");
        assert!(!r.allows("/early"));
        assert!(r.allows("/late"), "rules past 500 KiB are not read");
        let r = RobotsRules::parse(b"\xef\xbb\xbfUser-agent: *\r\nDisallow: /x\r\n\xff\xfe: junk\n", "SlopNinjaBot");
        assert!(!r.allows("/x"));
    }

    #[test]
    fn wildcard_matching_has_no_blow_up() {
        assert!(matches("/*a*b*c", "/zzazzbzzc"));
        assert!(!matches("/*a*b*c$", "/zzazzbzzcz"));
        assert!(matches("/*", "/"));
        assert!(matches("*", ""));
        assert!(!matches("/a$", "/ab"));
        let pattern = format!("/{}b", "*a".repeat(200));
        let path = format!("/{}", "a".repeat(8000));
        assert!(!matches(&pattern, &path));
    }

    #[test]
    fn fixed_rules() {
        assert!(!RobotsRules::disallow_all().allows("/x"));
        assert!(RobotsRules::disallow_all().allows("/robots.txt"));
        assert!(RobotsRules::allow_all().allows("/x"));
    }
}

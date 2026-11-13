//! `url_check_v1`: the input checks (risk F1), run on the submitted URL and
//! again on every redirect hop.
//!
//! Every check reads the URL as the WHATWG parser left it: IDNA has mapped
//! the host to its A-label form, percent-encoded host bytes are decoded, and
//! any host that parses as an IPv4 address in any notation (decimal,
//! octal, hex, short forms, a trailing dot) is an IPv4 host. A host is
//! therefore compared only after parsing, never as typed.

use crate::capability::Capability;
use crate::fixture::FixturePrefix;
use crate::psl;
use url::{Host, Url};

/// The longest URL accepted, in octets: the length RFC 9110 section 4.1
/// recommends every recipient support, so a longer one cannot be expected
/// to fetch.
pub const MAX_URL_BYTES: usize = 8_000;

/// Our own hosts and the platform hosts a page must never come from; each
/// covers its subdomains. The site's configured origins are added by
/// [`Policy::from_origins`].
pub const OWN_SUFFIXES: &[&str] = &["slop.ninja", "workers.dev", "r2.dev", "r2.cloudflarestorage.com"];

/// Top-level labels reserved for local, test or non-DNS use (RFC 6761,
/// RFC 6762, RFC 7686, RFC 9476, RFC 8375 under `arpa`, and `internal`).
pub const RESERVED_TLDS: &[&str] = &["localhost", "local", "internal", "arpa", "test", "invalid", "example", "onion", "alt"];

/// The production site host, whose `/d/<slug>` links answer their slug.
pub const SITE_HOST: &str = "slop.ninja";

/// Why a URL was refused. [`Refusal::code`] is the stable code for logs and
/// tests; [`Refusal::public_code`] is the code the pages print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    TooLong,
    /// Not a URL, or one with control characters or an empty host label.
    Invalid,
    Scheme,
    Userinfo,
    Port,
    IpLiteral,
    SingleLabel,
    Reserved,
    /// The top-level label is in no Public Suffix List rule.
    UnknownSuffix,
    /// The host is itself a public suffix (no registrable domain).
    PublicSuffix,
    OwnHost,
    /// A platform whose posts are paste-only (Reddit, LinkedIn, Facebook,
    /// Instagram, Threads) or not yet read by URL (X).
    PasteOnly,
    /// A link on an X host that is not a post (a profile, search, a `t.co`
    /// short link, `twimg.com` media).
    XNotAPost,
    Capability(Capability),
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::TooLong => "url_too_long",
            Refusal::Invalid => "url_invalid",
            Refusal::Scheme => "url_scheme",
            Refusal::Userinfo => "url_userinfo",
            Refusal::Port => "url_port",
            Refusal::IpLiteral => "url_ip_literal",
            Refusal::SingleLabel => "url_single_label",
            Refusal::Reserved => "url_reserved",
            Refusal::UnknownSuffix => "url_unknown_suffix",
            Refusal::PublicSuffix => "url_public_suffix",
            Refusal::OwnHost => "url_own_host",
            Refusal::PasteOnly => "paste_only",
            Refusal::XNotAPost => "x_not_a_post",
            Refusal::Capability(c) => c.code(),
        }
    }

    /// The refusal code the pages print: `capability_url` for every
    /// capability rule, `paste_only`, `x_not_a_post`, and `bad_url` for the
    /// input checks.
    pub fn public_code(self) -> &'static str {
        match self {
            Refusal::Capability(_) => "capability_url",
            Refusal::PasteOnly => "paste_only",
            Refusal::XNotAPost => "x_not_a_post",
            _ => "bad_url",
        }
    }
}

/// The deployment's own hosts and allowances (origins come from config).
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Hosts whose `/d/<slug>` path answers with the slug.
    site_hosts: Vec<String>,
    /// Hosts refused with their subdomains, beyond [`OWN_SUFFIXES`].
    own_hosts: Vec<String>,
    fixture: Option<FixturePrefix>,
    #[cfg(feature = "dev")]
    dev_host: Option<String>,
}

impl Policy {
    /// The production policy: `slop.ninja` and its subdomains are ours, and
    /// its `/d/<slug>` links answer their slug.
    pub fn production() -> Policy {
        Policy { site_hosts: vec![SITE_HOST.to_owned()], ..Policy::default() }
    }

    /// The production policy plus the hosts of the configured site and
    /// secret-host origins (`SITE_ORIGIN`, `SECRET_ORIGIN`). An origin that
    /// does not parse adds nothing.
    pub fn from_origins(site_origin: Option<&str>, secret_origin: Option<&str>) -> Policy {
        let mut p = Policy::production();
        if let Some(h) = site_origin.and_then(origin_host) {
            if !p.site_hosts.contains(&h) {
                p.site_hosts.push(h.clone());
            }
            p.own_hosts.push(h);
        }
        if let Some(h) = secret_origin.and_then(origin_host) {
            p.own_hosts.push(h);
        }
        p
    }

    /// Adds the staging fixture allowance.
    pub fn with_fixture(mut self, f: FixturePrefix) -> Policy {
        self.fixture = Some(f);
        self
    }

    pub fn fixture(&self) -> Option<&FixturePrefix> {
        self.fixture.as_ref()
    }

    /// Dev builds only: URLs on `host` (the local fixture site) pass the
    /// checks, any port and either scheme.
    #[cfg(feature = "dev")]
    pub fn with_dev_host(mut self, host: &str) -> Policy {
        self.dev_host = Some(host.trim().trim_end_matches('.').to_ascii_lowercase());
        self
    }

    fn is_own(&self, host: &str) -> bool {
        OWN_SUFFIXES.iter().copied().chain(self.own_hosts.iter().map(String::as_str)).any(|o| under(host, o))
    }

    fn is_site(&self, host: &str) -> bool {
        self.site_hosts.iter().any(|h| h == host)
    }
}

fn origin_host(origin: &str) -> Option<String> {
    let u = Url::parse(origin.trim()).ok()?;
    match u.host() {
        Some(Host::Domain(d)) => Some(d.trim_end_matches('.').to_owned()),
        _ => None,
    }
}

/// Whether `host` is `suffix` or a subdomain of it.
pub fn under(host: &str, suffix: &str) -> bool {
    host == suffix
        || (host.len() > suffix.len() && host.ends_with(suffix) && host.as_bytes().get(host.len() - suffix.len() - 1) == Some(&b'.'))
}

/// A URL that passed the input checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedUrl {
    /// The parsed URL (not yet canonical).
    pub url: Url,
    /// The host without a trailing dot.
    pub host: String,
    pub registrable: String,
    /// Passed through the staging fixture allowance.
    pub fixture: bool,
}

/// What [`check`] answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    Url(CheckedUrl),
    /// A `/d/<slug>` link on the site.
    Slug(String),
}

/// The input checks (`url_check_v1`).
pub fn check(input: &str, policy: &Policy) -> Result<Checked, Refusal> {
    // The WHATWG parser strips leading and trailing C0 controls and spaces
    // and silently drops tabs and newlines inside; inside a URL they are
    // refused instead, so no two readers see different URLs.
    let raw = input.trim_matches(|c: char| c <= ' ');
    if raw.len() > MAX_URL_BYTES {
        return Err(Refusal::TooLong);
    }
    if raw.chars().any(|c| c.is_control() || c == ' ') {
        return Err(Refusal::Invalid);
    }
    let url = Url::parse(raw).map_err(|_| Refusal::Invalid)?;
    if url.as_str().len() > MAX_URL_BYTES {
        return Err(Refusal::TooLong);
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refusal::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Refusal::Userinfo);
    }
    let host = match url.host() {
        Some(Host::Domain(d)) => d,
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => return Err(Refusal::IpLiteral),
        None => return Err(Refusal::Invalid),
    };
    let host = host.strip_suffix('.').unwrap_or(host).to_owned();
    #[cfg(feature = "dev")]
    if policy.dev_host.as_deref() == Some(host.as_str()) {
        return Ok(Checked::Url(CheckedUrl { url, registrable: host.clone(), host, fixture: false }));
    }
    if url.port().is_some() {
        return Err(Refusal::Port);
    }
    if host.is_empty() || host.split('.').any(str::is_empty) || !host.is_ascii() {
        return Err(Refusal::Invalid);
    }
    let tld = host.rsplit('.').next().unwrap_or("");
    // The parser turns every numeric host into an IPv4 address or fails;
    // this is the belt to that: a numeric or hex last label is never a name.
    if tld.bytes().all(|b| b.is_ascii_digit()) || tld.starts_with("0x") {
        return Err(Refusal::IpLiteral);
    }
    if RESERVED_TLDS.contains(&tld) {
        return Err(Refusal::Reserved);
    }
    if !host.contains('.') {
        return Err(Refusal::SingleLabel);
    }
    let mut fixture = false;
    if policy.is_own(&host) {
        if policy.is_site(&host)
            && let Some(slug) = site_slug(&url)
        {
            return Ok(Checked::Slug(slug));
        }
        if policy.fixture.as_ref().is_some_and(|f| f.covers(&url)) {
            fixture = true;
        } else {
            return Err(Refusal::OwnHost);
        }
    }
    if !psl::known_tld(tld) {
        return Err(Refusal::UnknownSuffix);
    }
    let registrable = psl::registrable_domain(&host).ok_or(Refusal::PublicSuffix)?.to_owned();
    Ok(Checked::Url(CheckedUrl { url, host, registrable, fixture }))
}

/// A public page link on the site: `/d/` and a 7-character slug of letters
/// and digits, with an optional trailing slash, no query. The slug is
/// returned lowercased; the detect Worker folds and validates it.
fn site_slug(url: &Url) -> Option<String> {
    if url.query().is_some() {
        return None;
    }
    let rest = url.path().strip_prefix("/d/")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    (rest.len() == 7 && rest.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| rest.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(u: &str) -> Refusal {
        match check(u, &Policy::production()) {
            Err(r) => r,
            Ok(c) => panic!("{u} passed: {c:?}"),
        }
    }

    fn passes(u: &str) -> CheckedUrl {
        match check(u, &Policy::production()) {
            Ok(Checked::Url(c)) => c,
            other => panic!("{u}: {other:?}"),
        }
    }

    #[test]
    fn every_ip_literal_form_is_refused() {
        for u in [
            "http://127.0.0.1/",
            "http://127.1/",
            "http://127.0.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://0x7F.0.0.1/",
            "http://0177.0.0.1/",
            "http://0177.1/",
            "http://017700000001/",
            "http://127.0.0.1./",
            "http://%31%32%37.0.0.1/",
            "http://\u{ff11}\u{ff12}\u{ff17}.0.0.1/",
            "http://0/",
            "http://1.1.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:7f00:1]/",
            "http://[0:0:0:0:0:ffff:127.0.0.1]/",
            "http://[2001:db8::1]/",
            "http://[fe80::1%25eth0]/",
            "https://10.0.0.1:443/",
        ] {
            let r = match check(u, &Policy::production()) {
                Err(r) => r,
                Ok(c) => panic!("{u} passed: {c:?}"),
            };
            assert!(matches!(r, Refusal::IpLiteral | Refusal::Invalid), "{u}: {r:?}");
        }
        // The decimal, octal and hex forms parse to an IPv4 host: refused as
        // IP literals, never as names.
        for u in ["http://2130706433/", "http://0x7f000001/", "http://0177.0.0.1/", "http://127.1/"] {
            assert_eq!(refused(u), Refusal::IpLiteral, "{u}");
        }
    }

    #[test]
    fn idna_maps_hosts_before_any_comparison() {
        assert_eq!(passes("https://B\u{fc}cher.de/x").host, "xn--bcher-kva.de");
        assert_eq!(passes("https://xn--bcher-kva.de/x").registrable, "xn--bcher-kva.de");
        // Full-width letters and a soft hyphen map to our own host.
        assert_eq!(refused("https://\u{ff53}\u{ff4c}\u{ff4f}\u{ff50}.ninja/"), Refusal::OwnHost);
        assert_eq!(refused("https://slop\u{ad}.ninja/"), Refusal::OwnHost);
        assert_eq!(refused("https://SLOP.NINJA/"), Refusal::OwnHost);
        // A percent-encoded host decodes before the check.
        assert_eq!(refused("https://slop%2Eninja/"), Refusal::OwnHost);
        // An IDN under a reserved suffix.
        assert_eq!(refused("https://\u{e9}cole.test/"), Refusal::Reserved);
        // Hosts that IDNA refuses.
        assert_eq!(refused("https://xn--a.com/"), Refusal::Invalid);
        assert_eq!(refused("https://exa mple.com/"), Refusal::Invalid);
    }

    #[test]
    fn ports_and_schemes() {
        assert_eq!(passes("https://example.com:443/").url.port(), None);
        assert_eq!(passes("http://example.com:80/").url.port(), None);
        for u in ["https://example.com:80/", "http://example.com:443/", "https://example.com:8443/", "https://example.com:0/"] {
            assert_eq!(refused(u), Refusal::Port, "{u}");
        }
        for u in [
            "ftp://example.com/",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "ws://example.com/",
            "gopher://example.com/",
        ] {
            assert!(matches!(refused(u), Refusal::Scheme | Refusal::Invalid), "{u}");
        }
    }

    #[test]
    fn userinfo_is_refused() {
        for u in [
            "https://user@example.com/",
            "https://user:pass@example.com/",
            "https://:pass@example.com/",
            "https://slop.ninja@evil.example/",
        ] {
            assert_eq!(refused(u), Refusal::Userinfo, "{u}");
        }
    }

    #[test]
    fn reserved_suffixes_and_single_labels() {
        for u in [
            "https://localhost/",
            "https://a.localhost/",
            "https://printer.local/",
            "https://db.internal/",
            "https://1.0.0.127.in-addr.arpa/",
            "https://router.home.arpa/",
            "https://example.test/",
            "https://example.invalid/",
            "https://www.example/",
            "https://abc.onion/",
            "https://name.alt/",
        ] {
            assert_eq!(refused(u), Refusal::Reserved, "{u}");
        }
        assert_eq!(refused("https://intranet/"), Refusal::SingleLabel);
        assert_eq!(refused("https://com/"), Refusal::SingleLabel);
        assert_eq!(refused("https://intranet.corp/"), Refusal::UnknownSuffix);
        assert_eq!(refused("https://nas.lan/"), Refusal::UnknownSuffix);
        assert_eq!(refused("https://co.uk/"), Refusal::PublicSuffix);
        assert_eq!(refused("https://github.io/"), Refusal::PublicSuffix);
        assert_eq!(refused("https://a..example.com/"), Refusal::Invalid);
    }

    #[test]
    fn our_own_hosts_are_refused_except_public_page_links() {
        for u in [
            "https://slop.ninja/",
            "https://slop.ninja/detect/",
            "https://s.slop.ninja/abcdefghijklmnopqrst",
            "https://mail.slop.ninja/",
            "https://slop.ninja./",
            "https://anything.workers.dev/",
            "https://pub-123.r2.dev/a.txt",
            "https://acct.r2.cloudflarestorage.com/bucket/key",
            "https://slop.ninja/d/abc12345",
            "https://slop.ninja/d/abc1234?x=1",
            "https://s.slop.ninja/d/abc1234",
        ] {
            assert_eq!(refused(u), Refusal::OwnHost, "{u}");
        }
        let p = Policy::production();
        assert_eq!(check("https://slop.ninja/d/Ab3dE7g", &p), Ok(Checked::Slug("ab3de7g".into())));
        assert_eq!(check("https://SLOP.ninja./d/ab3de7g/", &p), Ok(Checked::Slug("ab3de7g".into())));
        // Suffix matching is by label: a look-alike registrable domain passes.
        assert_eq!(passes("https://notslop.ninja/").registrable, "notslop.ninja");
        assert_eq!(passes("https://slop.ninja.example.com/").registrable, "example.com");
    }

    #[test]
    fn configured_origins_add_their_hosts() {
        let p = Policy::from_origins(Some("https://site.example.org"), Some("https://secret.example.org"));
        assert_eq!(check("https://site.example.org/d/ab3de7g", &p), Ok(Checked::Slug("ab3de7g".into())));
        assert_eq!(check("https://site.example.org/x", &p), Err(Refusal::OwnHost));
        assert_eq!(check("https://secret.example.org/d/ab3de7g", &p), Err(Refusal::OwnHost));
        assert_eq!(check("https://slop.ninja/d/ab3de7g", &p), Ok(Checked::Slug("ab3de7g".into())));
    }

    #[test]
    fn length_and_controls() {
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert_eq!(refused(&long), Refusal::TooLong);
        let fits = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES - 20));
        assert_eq!(passes(&fits).host, "example.com");
        for u in ["https://exa\tmple.com/", "https://example.com/a\nb", "https://example.com/a\rb", "https://example.com/a b"] {
            assert_eq!(refused(u), Refusal::Invalid, "{u:?}");
        }
        // Surrounding whitespace is trimmed, as the parser does.
        assert_eq!(passes("  https://example.com/ \n").host, "example.com");
    }

    #[test]
    fn under_matches_whole_labels() {
        assert!(under("slop.ninja", "slop.ninja"));
        assert!(under("a.b.slop.ninja", "slop.ninja"));
        assert!(!under("notslop.ninja", "slop.ninja"));
        assert!(!under("ninja", "slop.ninja"));
    }
}

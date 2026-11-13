//! Origins from config.
//!
//! No origin is ever read from a request's `Host`: the Workers take
//! `SITE_ORIGIN` and `SECRET_ORIGIN` from their vars, and everything that
//! names a host derives from them (the `return_to` base, the mail link, the
//! wallet message's domain and URI lines, the CSRF and cookie origin checks).
//! A configured value must already be the origin's canonical serialization,
//! so a typo fails at load rather than in a comparison.

use url::Url;

/// A validated origin: `https://host[:port]` in canonical form. With the
/// `dev` feature, `http` is also accepted for a loopback host (local
/// `wrangler dev`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    text: String,
    url: Url,
}

/// Why a configured origin was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginError {
    /// It does not parse as an absolute URL.
    Unparsable,
    /// The scheme is not `https` (or, under `dev`, `http` on loopback).
    Scheme,
    /// It has a path, query, fragment or user info, or is not in canonical
    /// form (for example a trailing slash or upper-case host).
    NotCanonical,
}

impl Origin {
    /// Parses a configured origin.
    pub fn parse(text: &str) -> Result<Self, OriginError> {
        let url = Url::parse(text).map_err(|_| OriginError::Unparsable)?;
        match url.scheme() {
            "https" => {}
            "http" if cfg!(feature = "dev") && is_loopback(&url) => {}
            _ => return Err(OriginError::Scheme),
        }
        if url.host_str().is_none_or(str::is_empty)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(OriginError::NotCanonical);
        }
        let canonical = url.origin().ascii_serialization();
        if canonical != text {
            return Err(OriginError::NotCanonical);
        }
        Ok(Self {
            text: canonical,
            url,
        })
    }

    /// The canonical serialization, for example `https://site.example`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The origin's root URL (`https://site.example/`), the base relative
    /// references are joined against.
    #[must_use]
    pub fn root(&self) -> &Url {
        &self.url
    }

    /// The host with its port when it is not the scheme's default
    /// (`site.example`, or `127.0.0.1:8787` in local development): the domain
    /// line of a wallet sign-in message, which a wallet compares with the
    /// requesting page's host, port included.
    #[must_use]
    pub fn authority(&self) -> &str {
        // The canonical form is `<scheme>://<authority>`.
        let scheme_len = self.url.scheme().len() + "://".len();
        &self.text[scheme_len..]
    }

    /// Whether `other` is exactly this origin's serialization.
    #[must_use]
    pub fn is(&self, other: &str) -> bool {
        self.text == other
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The two public origins every auth decision derives from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthOrigins {
    /// `SITE_ORIGIN`: the site host, where accounts and cookies live.
    pub site: Origin,
    /// `SECRET_ORIGIN`: the cookie-free secret-link host.
    pub secret: Origin,
}

impl AuthOrigins {
    /// Parses both origins; they must differ.
    pub fn parse(site: &str, secret: &str) -> Result<Self, OriginError> {
        let site = Origin::parse(site)?;
        let secret = Origin::parse(secret)?;
        if site == secret {
            return Err(OriginError::NotCanonical);
        }
        Ok(Self { site, secret })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_https_only() {
        let o = Origin::parse("https://site.example").unwrap();
        assert_eq!(o.as_str(), "https://site.example");
        assert_eq!(o.authority(), "site.example");
        assert_eq!(o.root().as_str(), "https://site.example/");
        assert_eq!(
            Origin::parse("https://example.test:8443")
                .unwrap()
                .authority(),
            "example.test:8443"
        );
        for bad in [
            "https://site.example/",
            "https://Site.example",
            "https://site.example:443",
            "https://site.example/x",
            "https://u@site.example",
            "https://site.example?x",
            "https://site.example#x",
            "site.example",
            "ftp://site.example",
            "",
        ] {
            assert!(Origin::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            Origin::parse("http://site.example"),
            Err(OriginError::Scheme)
        );
    }

    #[test]
    fn loopback_http_only_under_dev() {
        let r = Origin::parse("http://127.0.0.1:8787");
        if cfg!(feature = "dev") {
            assert_eq!(r.unwrap().authority(), "127.0.0.1:8787");
        } else {
            assert_eq!(r, Err(OriginError::Scheme));
        }
        assert_eq!(
            Origin::parse("http://10.0.0.1:8787"),
            Err(OriginError::Scheme)
        );
    }

    #[test]
    fn site_and_secret_differ() {
        assert!(AuthOrigins::parse("https://site.example", "https://s.site.example").is_ok());
        assert!(AuthOrigins::parse("https://site.example", "https://site.example").is_err());
    }
}

//! The staging-only fixture allowance (`URL_FIXTURE_PREFIX`).
//!
//! A staging drill checks URL pages against synthetic fixture pages served
//! from one unlisted preview path of the production site. The site is our
//! own host, so the input checks refuse it; this allowance admits exactly
//! one prefix of the form `https://slop.ninja/preview/<token>/fixtures/`
//! and nothing else on our hosts. The Workers read it only in their staging
//! environment, and the production configuration cannot carry it (each
//! Worker's source test).

use url::Url;

/// The one host a fixture prefix may name.
pub const FIXTURE_HOST: &str = "slop.ninja";

/// A validated fixture prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixturePrefix {
    prefix: String,
}

impl FixturePrefix {
    /// Parses the var's value: exactly `https://slop.ninja/preview/<token>/fixtures/`
    /// with a token of 8 to 128 characters from `[A-Za-z0-9_-]`. Anything
    /// else (another host, path, scheme, a query, a placeholder) is `None`.
    pub fn parse(value: &str) -> Option<FixturePrefix> {
        let v = value.trim();
        let rest = v.strip_prefix("https://slop.ninja/preview/")?;
        let token = rest.strip_suffix("/fixtures/")?;
        let ok = (8..=128).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !ok {
            return None;
        }
        // The parser must leave it unchanged (no normalization surprises).
        let u = Url::parse(v).ok()?;
        (u.as_str() == v).then(|| FixturePrefix { prefix: v.to_owned() })
    }

    /// Whether `url` (as parsed, so dot segments are already resolved)
    /// lies under the prefix.
    pub fn covers(&self, url: &Url) -> bool {
        url.as_str().starts_with(&self.prefix) && url.host_str() == Some(FIXTURE_HOST)
    }

    /// The prefix's path length in bytes (`/preview/<token>/fixtures/`):
    /// the capability rule skips this part, which holds the preview token.
    pub fn path_len(&self) -> usize {
        self.prefix.len() - "https://slop.ninja".len()
    }

    pub fn as_str(&self) -> &str {
        &self.prefix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_shape_parses() {
        let ok = "https://slop.ninja/preview/fixturetoken0001/fixtures/";
        assert!(FixturePrefix::parse(ok).is_some());
        for bad in [
            "",
            "<URL_FIXTURE_PREFIX>",
            "https://slop.ninja/preview/fixturetoken0001/fixtures",
            "https://slop.ninja/preview/fixturetoken0001/other/",
            "https://s.slop.ninja/preview/fixturetoken0001/fixtures/",
            "http://slop.ninja/preview/fixturetoken0001/fixtures/",
            "https://slop.ninja/preview/short/fixtures/",
            "https://slop.ninja/preview/a/../b/fixtures/",
            "https://slop.ninja/preview/fixturetoken%2e%2e/fixtures/",
            "https://slop.ninja/preview/fixturetoken0001/fixtures/?x",
            "https://example.com/preview/fixturetoken0001/fixtures/",
        ] {
            assert!(FixturePrefix::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn covers_resolves_dot_segments_first() {
        let f = FixturePrefix::parse("https://slop.ninja/preview/fixturetoken0001/fixtures/").unwrap();
        assert!(f.covers(&Url::parse("https://slop.ninja/preview/fixturetoken0001/fixtures/a.html").unwrap()));
        assert!(!f.covers(&Url::parse("https://slop.ninja/preview/fixturetoken0001/fixtures/../../x").unwrap()));
        assert!(!f.covers(&Url::parse("https://slop.ninja/preview/fixturetoken0001/fixtures/%2e%2e/x").unwrap()));
        assert!(!f.covers(&Url::parse("https://slop.ninja/d/abc1234").unwrap()));
    }
}

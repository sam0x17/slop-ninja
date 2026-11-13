//! The two public origins, from config.
//!
//! Every Worker that names a public host takes both origins from its vars
//! `SITE_ORIGIN` and `SECRET_ORIGIN`: the `X-SN-Host` values, the
//! `return_to` base, the mail link origin, the sign-in message's domain and
//! URI, the CSP's secret-host `form-action` and the cookie and CSRF origin
//! checks all derive from them. No origin is compiled in: each deployment
//! (production, a staging copy, local development) sets its own.

use crate::codes::HostRole;

/// One `https` origin: a lowercase host of LDH labels and an optional port.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    text: String,
    host_len: usize,
}

/// Why an origin var was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginError {
    /// Not `https://<host>[:<port>]` with nothing after it.
    Form,
    /// The host is not lowercase LDH labels of 1 to 63 octets, at most 253
    /// in all.
    Host,
    /// The port is not 1 to 65535 in canonical decimal.
    Port,
    /// The site and secret origins are the same host.
    SameHost,
}

impl Origin {
    /// Parses `https://<host>[:<port>]`, with no path, query, fragment or
    /// userinfo, no trailing dot and no uppercase.
    pub fn parse(s: &str) -> Result<Origin, OriginError> {
        let rest = s.strip_prefix("https://").ok_or(OriginError::Form)?;
        if rest.is_empty() || rest.contains(['/', '?', '#', '@', '\\']) {
            return Err(OriginError::Form);
        }
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        if host.is_empty() || host.len() > 253 {
            return Err(OriginError::Host);
        }
        for label in host.split('.') {
            let ok = !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !ok {
                return Err(OriginError::Host);
            }
        }
        if let Some(p) = port {
            let canonical = !p.is_empty() && !p.starts_with('0') && p.bytes().all(|b| b.is_ascii_digit());
            match p.parse::<u32>() {
                Ok(n) if canonical && (1..=65535).contains(&n) => {}
                _ => return Err(OriginError::Port),
            }
        }
        Ok(Origin { text: s.to_owned(), host_len: host.len() })
    }

    /// `https://<host>[:<port>]`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The host, without the port.
    pub fn host(&self) -> &str {
        &self.text["https://".len().."https://".len() + self.host_len]
    }

    /// The host and port as the `Host` header carries them.
    pub fn authority(&self) -> &str {
        &self.text["https://".len()..]
    }

    /// The origin followed by `path`, which must start with `/`.
    pub fn url(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'));
        format!("{}{}", self.text, path)
    }
}

/// The site and secret-host origins.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origins {
    pub site: Origin,
    pub secret: Origin,
}

impl Origins {
    /// Parses the two vars; the hosts must differ.
    pub fn from_vars(site: &str, secret: &str) -> Result<Origins, OriginError> {
        let o = Origins { site: Origin::parse(site)?, secret: Origin::parse(secret)? };
        if o.site.host() == o.secret.host() {
            return Err(OriginError::SameHost);
        }
        Ok(o)
    }

    /// The origin of a host role.
    pub fn of(&self, role: HostRole) -> &Origin {
        match role {
            HostRole::Site => &self.site,
            HostRole::Secret => &self.secret,
        }
    }

    /// The `X-SN-Host` value of a role: its authority.
    pub fn sn_host(&self, role: HostRole) -> &str {
        self.of(role).authority()
    }

    /// The role whose `X-SN-Host` value is `value`, exactly.
    pub fn role_of_sn_host(&self, value: &str) -> Option<HostRole> {
        if value == self.site.authority() {
            Some(HostRole::Site)
        } else if value == self.secret.authority() {
            Some(HostRole::Secret)
        } else {
            None
        }
    }

    /// The absolute URL of a public page, `<site>/d/<slug>`.
    pub fn public_page_url(&self, slug: &str) -> String {
        self.site.url(&format!("/d/{slug}"))
    }

    /// The absolute URL of a secret page, `<secret>/<slug>`.
    pub fn secret_page_url(&self, slug: &str) -> String {
        self.secret.url(&format!("/{slug}"))
    }
}

//! `capability_url_v1`: URLs that act as credentials or one-time actions
//! are refused before any slug exists (risks F6 and F7).
//!
//! A canonical URL is refused when any of these holds:
//!
//! 1. **Share service.** Its host is a document, storage or paste share
//!    service ([`SHARE_HOSTS`], each covering its subdomains).
//! 2. **Credential-named parameter.** A query parameter's name (lowercased,
//!    percent-decoded) carries a credential or a one-time action:
//!    [`CREDENTIAL_PARAMS`], or a name ending in `token`, `_key`, `-key`,
//!    `secret`, `signature`, `password`, `_sig` or `_otp`, or starting with
//!    `x-amz-`, `x-goog-` or `x-ms-`.
//! 3. **Side-effect path.** A path segment (lowercased, percent-decoded) is
//!    one of [`SIDE_EFFECT_SEGMENTS`]: fetching a password-reset,
//!    verification, magic sign-in, unsubscribe or invite link would consume
//!    it, and its page might show private data.
//! 4. **Random-looking run.** A path segment or query value (percent-decoded)
//!    holds a maximal run of `[A-Za-z0-9]` with at least one digit whose
//!    estimated entropy, its length times log2 of its alphabet, is at least
//!    80 bits; or a UUID (128 bits). The alphabet is the smallest of digits
//!    (10), single-case hex (16), single-case letters and digits (36) and
//!    mixed case (62). Runs of letters alone never count, so article slugs
//!    pass. 80 bits is the floor for an unguessable link: a shorter token
//!    could not serve as a capability anyway, and common public ids sit
//!    below it (Medium's 12-hex suffix 48 bits, numeric ids up to 24 digits,
//!    Bluesky record keys 67, YouTube ids 66, X status ids 63). Hosts whose
//!    public ids exceed it are on the versioned allow-list [`ALLOW`].

use crate::canon::param_name;
use url::Url;

/// The rule's tables' version (the share hosts, names, segments and
/// allow-list below).
pub const CAPABILITY_TABLES_V: &str = "capability_tables_v1";

/// Which rule refused a URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    ShareService,
    CredentialParam,
    SideEffectPath,
    RandomRun,
}

impl Capability {
    pub fn code(self) -> &'static str {
        match self {
            Capability::ShareService => "capability_share_service",
            Capability::CredentialParam => "capability_credential_param",
            Capability::SideEffectPath => "capability_side_effect_path",
            Capability::RandomRun => "capability_random_run",
        }
    }
}

/// Share services: documents, storage, transfers and pastes.
pub const SHARE_HOSTS: &[&str] = &[
    "docs.google.com",
    "drive.google.com",
    "drive.usercontent.google.com",
    "dropbox.com",
    "dropboxusercontent.com",
    "db.tt",
    "onedrive.live.com",
    "1drv.ms",
    "sharepoint.com",
    "box.com",
    "boxcloud.com",
    "icloud.com",
    "wetransfer.com",
    "we.tl",
    "mega.nz",
    "mega.io",
    "docsend.com",
    "file.io",
    "transfer.sh",
    "gist.github.com",
    "pastebin.com",
    "paste.ee",
    "hastebin.com",
    "ghostbin.com",
    "privatebin.net",
    "0bin.net",
    "controlc.com",
    "justpaste.it",
    "dpaste.com",
    "dpaste.org",
    "rentry.co",
    "rentry.org",
];

/// Query parameter names that carry a credential or a one-time action.
pub const CREDENTIAL_PARAMS: &[&str] = &[
    "key",
    "token",
    "access_token",
    "accesstoken",
    "refresh_token",
    "id_token",
    "auth_token",
    "authtoken",
    "api_key",
    "apikey",
    "authkey",
    "share",
    "sharekey",
    "share_key",
    "sig",
    "signature",
    "secret",
    "password",
    "passwd",
    "pwd",
    "pass",
    "session",
    "sessionid",
    "session_id",
    "sid",
    "jwt",
    "ticket",
    "code",
    "otp",
    "reset",
    "verify",
    "verification",
    "confirm",
    "confirmation",
    "unsubscribe",
    "invite",
    "invitation",
    "invite_code",
    "magic",
    "magic_link",
    "auth",
    "oobcode",
    "rlkey",
];

/// Path segments that name a one-time action.
pub const SIDE_EFFECT_SEGMENTS: &[&str] = &[
    "code",
    "reset",
    "verify",
    "confirm",
    "unsubscribe",
    "invite",
    "magic",
    "otp",
    "auth",
    "reset-password",
    "reset_password",
    "resetpassword",
    "password-reset",
    "password_reset",
    "verify-email",
    "verify_email",
    "email-verification",
    "confirm-email",
    "confirm_email",
    "email-confirmation",
    "magic-link",
    "magic_link",
    "magiclink",
    "accept-invite",
    "accept_invite",
    "invitation",
    "invitations",
    "activate",
    "activation",
];

/// Where public ids exceed the bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdShape {
    /// A UUID anywhere in a path segment.
    Uuid,
    /// A run of exactly 32 hex characters ending a path segment.
    Hex32Suffix,
    /// A `did:plc:` identifier (24 base32 characters) as a whole segment.
    DidPlc,
}

/// The allow-list (`capability_allow_v1`): a host (exact), a path prefix,
/// and the id shape its public pages carry.
pub const ALLOW: &[(&str, &str, IdShape)] = &[
    ("apnews.com", "/article/", IdShape::Hex32Suffix),
    ("www.ft.com", "/content/", IdShape::Uuid),
    ("www.washingtonpost.com", "/", IdShape::Uuid),
    ("bsky.app", "/profile/did:plc:", IdShape::DidPlc),
];

/// `capability_url_v1` on a canonical URL.
pub fn capability_url_v1(url: &Url) -> Result<(), Capability> {
    check_from(url, url.host_str().unwrap_or(""), 0)
}

/// The rule, with the first `skip` bytes of the path exempt (the staging
/// fixture prefix, which holds a preview token).
pub(crate) fn check_from(url: &Url, host: &str, skip: usize) -> Result<(), Capability> {
    let host = host.trim_end_matches('.');
    if SHARE_HOSTS.iter().any(|s| crate::check::under(host, s)) {
        return Err(Capability::ShareService);
    }
    if let Some(q) = url.query() {
        for piece in q.split('&').filter(|p| !p.is_empty()) {
            if credential_name(&param_name(piece)) {
                return Err(Capability::CredentialParam);
            }
            if let Some((_, value)) = piece.split_once('=')
                && random_run(&decode(value), None)
            {
                return Err(Capability::RandomRun);
            }
        }
    }
    let path = url.path();
    let allow: Option<IdShape> = ALLOW.iter().find(|(h, p, _)| *h == host && path.starts_with(p)).map(|(_, _, s)| *s);
    let tail = path.get(skip..).unwrap_or("");
    for seg in tail.split('/').filter(|s| !s.is_empty()) {
        let seg = decode(seg);
        if SIDE_EFFECT_SEGMENTS.contains(&seg.to_ascii_lowercase().as_str()) {
            return Err(Capability::SideEffectPath);
        }
        if random_run(&seg, allow) {
            return Err(Capability::RandomRun);
        }
    }
    Ok(())
}

fn decode(s: &str) -> String {
    percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8_lossy().into_owned()
}

fn credential_name(name: &str) -> bool {
    let name = name.trim();
    CREDENTIAL_PARAMS.contains(&name)
        || ["token", "_key", "-key", "secret", "signature", "password", "_sig", "_otp"].iter().any(|s| name.ends_with(s))
        || ["x-amz-", "x-goog-", "x-ms-"].iter().any(|p| name.starts_with(p))
}

/// The shortest run, by alphabet, whose entropy reaches 80 bits:
/// `ceil(80 / log2(alphabet))`.
const MIN_DIGITS: usize = 25; // 80 / 3.32 = 24.1
const MIN_HEX: usize = 20; // 80 / 4
const MIN_ALNUM: usize = 16; // 80 / 5.17 = 15.5
const MIN_MIXED: usize = 14; // 80 / 5.95 = 13.4
const _: () = assert!(MIN_DIGITS * 332 >= 8000 && (MIN_DIGITS - 1) * 333 < 8000);
const _: () = assert!(MIN_HEX * 4 >= 80 && (MIN_HEX - 1) * 4 < 80);
const _: () = assert!(MIN_ALNUM * 517 >= 8000 && (MIN_ALNUM - 1) * 517 < 8000);
const _: () = assert!(MIN_MIXED * 595 >= 8000 && (MIN_MIXED - 1) * 596 < 8000);

/// Whether a run of `[A-Za-z0-9]` reaches 80 bits.
pub(crate) fn run_reaches_bound(run: &[u8]) -> bool {
    let digits = run.iter().any(u8::is_ascii_digit);
    if !digits {
        return false;
    }
    let lower = run.iter().any(u8::is_ascii_lowercase);
    let upper = run.iter().any(u8::is_ascii_uppercase);
    let min = if !lower && !upper {
        MIN_DIGITS
    } else if lower && upper {
        MIN_MIXED
    } else if run.iter().all(u8::is_ascii_hexdigit) {
        MIN_HEX
    } else {
        MIN_ALNUM
    };
    run.len() >= min
}

fn is_uuid_at(b: &[u8], i: usize) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let mut j = i;
    for (n, len) in GROUPS.iter().enumerate() {
        let Some(g) = b.get(j..j + len) else { return false };
        if !g.iter().all(u8::is_ascii_hexdigit) {
            return false;
        }
        j += len;
        if n < 4 {
            if b.get(j) != Some(&b'-') {
                return false;
            }
            j += 1;
        }
    }
    // Not part of a longer alphanumeric run on either side.
    let before = i.checked_sub(1).and_then(|k| b.get(k)).is_some_and(u8::is_ascii_alphanumeric);
    let after = b.get(j).is_some_and(u8::is_ascii_alphanumeric);
    !before && !after
}

/// Whether `s` holds a random-looking run of 80 bits or more that the
/// allow-list shape `allow` does not cover.
pub(crate) fn random_run(s: &str, allow: Option<IdShape>) -> bool {
    let b = s.as_bytes();
    if allow == Some(IdShape::DidPlc)
        && let Some(id) = s.strip_prefix("did:plc:")
        && id.len() == 24
        && id.bytes().all(|c| c.is_ascii_lowercase() || (b'2'..=b'7').contains(&c))
    {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        if is_uuid_at(b, i) {
            if allow != Some(IdShape::Uuid) {
                return true;
            }
            i += 36;
            continue;
        }
        if !b.get(i).is_some_and(u8::is_ascii_alphanumeric) {
            i += 1;
            continue;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_alphanumeric) {
            i += 1;
        }
        let run = b.get(start..i).unwrap_or(&[]);
        let hex32_suffix = allow == Some(IdShape::Hex32Suffix) && i == b.len() && run.len() == 32 && run.iter().all(u8::is_ascii_hexdigit);
        if !hex32_suffix && run_reaches_bound(run) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(s: &str) -> Result<(), Capability> {
        capability_url_v1(&crate::canon_url_v1(&Url::parse(s).unwrap()))
    }

    #[test]
    fn public_ids_pass() {
        for u in [
            "https://medium.com/@writer/how-we-built-a-thing-1a2b3c4d5e6f",
            "https://writer.medium.com/how-we-built-a-thing-1a2b3c4d5e6f",
            "https://example.com/news/2024/05/01/the-long-article-slug-with-many-words-in-it",
            "https://example.com/story/123456789012345678901234",
            "https://example.com/p/12345?id=987654321&page=2",
            "https://x.com/someone/status/1790000000000000000",
            "https://bsky.app/profile/someone.bsky.social/post/3l6oveex3ii2l",
            "https://bsky.app/profile/did:plc:z72i7hdynmk6r22z27h6tvur/post/3l6oveex3ii2l",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://apnews.com/article/election-results-0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c",
            "https://www.ft.com/content/123e4567-e89b-12d3-a456-426614174000",
            "https://www.washingtonpost.com/politics/2019/01/01/123e4567-e89b-12d3-a456-426614174000_story.html",
            "https://example.com/wiki/List_of_Super_Bowl_champions",
            "https://example.com/blog/CamelCaseTitleWithoutDigits",
            "https://example.com/articles/a1b2c3d4e5f6g7h",
            "https://example.com/a?utm_source=abcdef0123456789abcdef0123456789",
            "https://example.com/coding/",
            "https://example.com/verified-accounts/",
        ] {
            assert_eq!(rule(u), Ok(()), "{u}");
        }
    }

    #[test]
    fn share_links_and_credentials_fail() {
        for u in [
            "https://docs.google.com/document/d/abc/edit?usp=sharing",
            "https://drive.google.com/file/d/abc/view",
            "https://www.dropbox.com/scl/fi/abc/file.pdf",
            "https://dl.dropboxusercontent.com/s/abc/file",
            "https://1drv.ms/w/s!abc",
            "https://contoso.sharepoint.com/:w:/r/sites/x",
            "https://app.box.com/s/abc",
            "https://www.icloud.com/iclouddrive/abc",
            "https://we.tl/t-abc",
            "https://mega.nz/file/abc",
            "https://pastebin.com/abcdefgh",
            "https://gist.github.com/someone/abc",
        ] {
            assert_eq!(rule(u), Err(Capability::ShareService), "{u}");
        }
        for u in [
            "https://example.com/a?token=x",
            "https://example.com/a?access_token=x",
            "https://example.com/a?Key=x",
            "https://example.com/a?share=1",
            "https://example.com/a?sig=x",
            "https://example.com/a?X-Amz-Signature=x&X-Amz-Credential=y",
            "https://example.com/a?X-Goog-Signature=x",
            "https://example.com/a?api-key=x",
            "https://example.com/a?reset_token=x",
            "https://example.com/a?code=x",
            "https://example.com/a?verify=1",
            "https://example.com/a?confirm",
            "https://example.com/a?unsubscribe=1",
            "https://example.com/a?invite=x",
            "https://example.com/a?magic=x",
            "https://example.com/a?otp=1",
            "https://example.com/a?auth=x",
            "https://example.com/a?reset=1",
            "https://example.com/a?%74oken=x",
        ] {
            assert_eq!(rule(u), Err(Capability::CredentialParam), "{u}");
        }
    }

    #[test]
    fn side_effect_paths_fail() {
        for u in [
            "https://example.com/reset/abc",
            "https://example.com/account/verify/abc",
            "https://example.com/email/confirm",
            "https://example.com/unsubscribe",
            "https://example.com/Unsubscribe/",
            "https://example.com/invite/abc",
            "https://example.com/auth/magic/abc",
            "https://example.com/otp",
            "https://example.com/code/abc",
            "https://example.com/reset-password/abc",
            "https://example.com/verify-email",
            "https://example.com/magic-link/abc",
            "https://example.com/activate/abc",
            "https://example.com/%72eset/abc",
        ] {
            assert_eq!(rule(u), Err(Capability::SideEffectPath), "{u}");
        }
    }

    #[test]
    fn random_runs_fail() {
        for u in [
            "https://example.com/p/a8F3kdl29QzxP0",
            "https://example.com/p/9f86d081884c7d659a2feaa0c55ad015",
            "https://example.com/p/1234567890123456789012345",
            "https://example.com/p/x7k2m9q4w8r3t6y1",
            "https://example.com/p/123e4567-e89b-12d3-a456-426614174000",
            "https://example.com/a?id=9f86d081884c7d659a2feaa0c55ad015",
            "https://example.com/a?next=%2Fdoc%2Fa8F3kdl29QzxP0",
            "https://www.ft.com/other/123e4567-e89b-12d3-a456-426614174000",
            "https://apnews.com/hub/0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c",
            "https://bsky.app/profile/did:plc:z72i7hdynmk6r22z27h6tvur7/post/3l6",
        ] {
            assert_eq!(rule(u), Err(Capability::RandomRun), "{u}");
        }
    }

    #[test]
    fn run_bounds() {
        assert!(!run_reaches_bound(b"123456789012345678901234"));
        assert!(run_reaches_bound(b"1234567890123456789012345"));
        assert!(!run_reaches_bound(b"0123456789abcdef012"));
        assert!(run_reaches_bound(b"0123456789abcdef0123"));
        assert!(!run_reaches_bound(b"abcdefghijklmn1"));
        assert!(run_reaches_bound(b"abcdefghijklmno1"));
        assert!(!run_reaches_bound(b"aBcDeFgHiJkL1"));
        assert!(run_reaches_bound(b"aBcDeFgHiJkLm1"));
        assert!(!run_reaches_bound(b"abcdefghijklmnopqrstuvwxyzABCDEFGH"));
    }
}

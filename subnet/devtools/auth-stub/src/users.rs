//! The stub's test users. Every value is synthetic: the addresses are under
//! the reserved `example.com` and `example.org` domains, and the ids are
//! made up. Each user has one identity per provider and a default avatar.

/// What the X avatar route serves for a user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Avatar {
    /// A 1x1 PNG.
    Png,
    /// JPEG magic bytes and padding.
    Jpeg,
    Gif,
    Webp,
    /// An SVG labelled `image/png` (dropped by the type check).
    Svg,
    /// An HTML body labelled `image/png` (dropped).
    Html,
    /// A PNG over 64 KiB (dropped).
    Big,
    /// A 302 to another image (not followed).
    Redirect,
    /// 404.
    Missing,
}

#[derive(Clone, Copy, Debug)]
pub struct User {
    pub key: &'static str,
    pub name: &'static str,
    pub email: Option<&'static str>,
    pub email_verified: bool,
    pub google_sub: &'static str,
    pub github_id: u64,
    pub github_login: &'static str,
    pub x_id: &'static str,
    pub x_username: &'static str,
    pub avatar: Avatar,
}

pub const USERS: &[User] = &[
    User {
        key: "alice",
        name: "Alice Example",
        email: Some("alice@example.com"),
        email_verified: true,
        google_sub: "110000000000000000001",
        github_id: 9_000_001,
        github_login: "alice-example",
        x_id: "1900000000000000001",
        x_username: "alice_example",
        avatar: Avatar::Png,
    },
    User {
        key: "bob",
        name: "Bob Example",
        email: Some("bob@example.com"),
        email_verified: true,
        google_sub: "110000000000000000002",
        github_id: 9_000_002,
        github_login: "bob-example",
        x_id: "1900000000000000002",
        x_username: "bob_example",
        avatar: Avatar::Jpeg,
    },
    User {
        key: "carol",
        name: "Carol Example",
        email: Some("carol@example.org"),
        email_verified: false,
        google_sub: "110000000000000000003",
        github_id: 9_000_003,
        github_login: "carol-example",
        x_id: "1900000000000000003",
        x_username: "carol_example",
        avatar: Avatar::Svg,
    },
    User {
        key: "dave",
        name: "Dave Example",
        email: None,
        email_verified: false,
        google_sub: "110000000000000000004",
        github_id: 9_000_004,
        github_login: "dave-example",
        x_id: "1900000000000000004",
        x_username: "dave_example",
        avatar: Avatar::Html,
    },
    User {
        key: "erin",
        name: "Erin Example",
        email: Some("erin@example.org"),
        email_verified: true,
        google_sub: "110000000000000000005",
        github_id: 9_000_005,
        github_login: "erin-example",
        x_id: "1900000000000000005",
        x_username: "erin_example",
        avatar: Avatar::Big,
    },
];

pub fn by_key(key: &str) -> Option<&'static User> {
    USERS.iter().find(|u| u.key == key)
}

/// The user a Google `login_hint` names (the key or the address).
pub fn by_hint(hint: &str) -> Option<&'static User> {
    let hint = hint.to_ascii_lowercase();
    USERS.iter().find(|u| u.key == hint || u.email == Some(hint.as_str()))
}

/// A 1x1 transparent PNG.
pub const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01,
    0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41,
    0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
    0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

/// The bytes and the upstream `Content-Type` of an avatar kind; `None`
/// for the kinds that answer no image (redirect, missing).
pub fn avatar_bytes(kind: Avatar) -> Option<(Vec<u8>, &'static str)> {
    let padded = |magic: &[u8], len: usize| {
        let mut v = magic.to_vec();
        v.resize(len, 0);
        v
    };
    Some(match kind {
        Avatar::Png => (PNG_1X1.to_vec(), "image/png"),
        Avatar::Jpeg => (padded(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00], 128), "image/jpeg"),
        Avatar::Gif => (padded(b"GIF89a", 64), "image/gif"),
        Avatar::Webp => {
            let mut v = b"RIFF".to_vec();
            v.extend_from_slice(&56u32.to_le_bytes());
            v.extend_from_slice(b"WEBPVP8 ");
            v.resize(64, 0);
            (v, "image/webp")
        }
        Avatar::Svg => (b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"48\" height=\"48\"></svg>".to_vec(), "image/png"),
        Avatar::Html => (b"<!doctype html><html><body>not an image</body></html>".to_vec(), "image/png"),
        Avatar::Big => (padded(PNG_1X1, 70 * 1024), "image/png"),
        Avatar::Redirect | Avatar::Missing => return None,
    })
}

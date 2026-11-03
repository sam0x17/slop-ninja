//! Conservative, line-delimited parser for the original corpus's pseudo-XML.
//!
//! It is not general XML: unescaped prose is legal in this dataset. Only whole
//! delimiter lines have structural meaning. Malformed spans are rejected rather
//! than repaired or joined to another passage. Offsets always address ZIP-entry
//! bytes, including their original CRLF line endings.

use crate::{DatePolicy, hash};
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Post {
    pub ordinal: usize,
    pub raw: Range<usize>,
    pub body: Range<usize>,
    pub date_raw: Option<String>,
    pub date: Option<String>,
    pub rejection: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct Text {
    /// Exact UTF-8 body with only leading/trailing whitespace excluded.
    pub exact: String,
    pub trimmed_span: Range<usize>,
    pub exact_sha256: String,
    /// Whitespace collapse is for duplicate detection only, never grammar input.
    pub normalized_sha256: String,
    pub has_c1_controls: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum Encoding {
    Latin1,
    Utf8,
}

impl Encoding {
    pub fn identity(self) -> &'static str {
        match self {
            Self::Latin1 => "iso-8859-1-byte-to-unicode-v1",
            Self::Utf8 => "declared-utf-8-strict-v1",
        }
    }
}

pub fn author_id(name: &str) -> Option<&str> {
    if name.starts_with('/')
        || name.contains('\\')
        || name
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | "..") || part.contains(':'))
    {
        return None;
    }
    let filename = name.rsplit('/').next()?;
    let parts: Vec<_> = filename.split('.').collect();
    if parts.len() != 6
        || parts[5] != "xml"
        || parts[0].is_empty()
        || !parts[0].bytes().all(|ch| ch.is_ascii_digit())
        || parts[1..5].iter().any(|part| part.is_empty())
    {
        return None;
    }
    Some(parts[0])
}

/// The corpus uses day,English-month,year, e.g. 31,May,2004.
pub fn date(value: &str) -> Option<String> {
    let parts: Vec<_> = value.split(',').collect();
    if parts.len() != 3
        || parts[0].is_empty()
        || parts[0].len() > 2
        || !parts[0].bytes().all(|ch| ch.is_ascii_digit())
        || parts[2].len() != 4
        || !parts[2].bytes().all(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    let year: u32 = parts[2].parse().ok()?;
    let month = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]
    .iter()
    .position(|month| month == &parts[1])?
        + 1;
    let day: u32 = parts[0].parse().ok()?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let maximum = match month {
        2 => 28 + u32::from(leap),
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if year == 0 || day == 0 || day > maximum {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

/// The published dataset loader uses Latin-1. This default maps every byte to
/// its Unicode code point without substitution; C1 controls remain observable.
/// An explicit UTF-8 declaration is honored strictly. Other encodings are not
/// guessed. This is ISO-8859-1, not the browser convention of using Windows-1252.
pub fn encoding(bytes: &[u8]) -> Option<Encoding> {
    let prefix = trim_ascii(bytes);
    if !prefix.starts_with(b"<?xml") {
        return Some(Encoding::Latin1);
    }
    let end = prefix.windows(2).position(|window| window == b"?>")?;
    let Ok(header) = std::str::from_utf8(&prefix[..end]) else {
        return None;
    };
    let lower = header.to_ascii_lowercase();
    let Some((_, rest)) = lower.split_once("encoding") else {
        return Some(Encoding::Latin1);
    };
    let rest = rest.trim_start().strip_prefix('=')?;
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|ch| matches!(ch, '\'' | '"'))?;
    match rest[1..].split_once(quote)?.0 {
        "utf-8" | "utf8" => Some(Encoding::Utf8),
        "iso-8859-1" | "latin-1" | "latin1" => Some(Encoding::Latin1),
        _ => None,
    }
}

pub fn parse(bytes: &[u8]) -> Vec<Post> {
    parse_with_date_policy(bytes, DatePolicy::default())
}

/// Explicit historical eligibility is available for reproducing legacy imports.
pub fn parse_with_date_policy(bytes: &[u8], policy: DatePolicy) -> Vec<Post> {
    let mut posts = Vec::new();
    let mut date_raw: Option<String> = None;
    let mut parsed_date: Option<String> = None;
    let mut date_rejection = Some("missing_date");
    let mut open: Option<Post> = None;
    let mut offset = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let start = offset;
        offset += line.len();
        let tag = trim_ascii(line);
        let date_line = tag.starts_with(b"<date") || tag.ends_with(b"</date>");
        if date_line || tag == b"<post>" || tag == b"</Blog>" {
            if let Some(mut unfinished) = open.take() {
                unfinished.raw.end = start;
                unfinished.body.end = start;
                unfinished.rejection = Some("unclosed_post");
                posts.push(unfinished);
            }
            if date_line {
                date_raw = tag
                    .strip_prefix(b"<date>")
                    .and_then(|value| value.strip_suffix(b"</date>"))
                    .and_then(|value| std::str::from_utf8(value).ok())
                    .map(str::to_owned);
                parsed_date = date_raw.as_deref().and_then(date);
                date_rejection = match parsed_date.as_deref() {
                    None => Some("malformed_date"),
                    Some(value) if policy.minimum().is_some_and(|minimum| value < minimum) => {
                        Some("before_1999")
                    }
                    Some(_) => None,
                };
            } else if tag == b"<post>" {
                open = Some(Post {
                    ordinal: posts.len(),
                    raw: start..offset,
                    body: offset..offset,
                    date_raw: date_raw.clone(),
                    date: parsed_date.clone(),
                    rejection: date_rejection,
                });
            }
        } else if tag == b"</post>" {
            if let Some(mut complete) = open.take() {
                complete.raw.end = offset;
                complete.body.end = start;
                posts.push(complete);
            } else {
                posts.push(Post {
                    ordinal: posts.len(),
                    raw: start..offset,
                    body: start..start,
                    date_raw: date_raw.clone(),
                    date: parsed_date.clone(),
                    rejection: Some("orphan_post_close"),
                });
            }
        }
    }
    if let Some(mut unfinished) = open {
        unfinished.raw.end = bytes.len();
        unfinished.body.end = bytes.len();
        unfinished.rejection = Some("unclosed_post");
        posts.push(unfinished);
    }
    posts
}

pub fn text(bytes: &[u8], span: Range<usize>, encoding: Encoding) -> Result<Text, &'static str> {
    let source = &bytes[span.clone()];
    let raw: String = match encoding {
        Encoding::Latin1 => source.iter().map(|byte| char::from(*byte)).collect(),
        Encoding::Utf8 => std::str::from_utf8(source)
            .map_err(|_| "invalid_declared_utf8")?
            .to_owned(),
    };
    let exact = raw.trim();
    if exact.is_empty() {
        return Err("empty_post");
    }
    let (start, end) = match encoding {
        Encoding::Latin1 => {
            let leading = raw.chars().take_while(|ch| ch.is_whitespace()).count();
            (
                span.start + leading,
                span.start + leading + exact.chars().count(),
            )
        }
        Encoding::Utf8 => {
            let start = span.start + raw.len() - raw.trim_start().len();
            (start, start + exact.len())
        }
    };
    let normalized = exact.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(Text {
        exact: exact.to_owned(),
        trimmed_span: start..end,
        exact_sha256: hash(exact.as_bytes()),
        normalized_sha256: hash(normalized.as_bytes()),
        has_c1_controls: raw
            .chars()
            .any(|ch| ('\u{0080}'..='\u{009f}').contains(&ch)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_dates_are_strict_and_age_floor_is_legacy_only() {
        assert_eq!(date("31,December,1998").as_deref(), Some("1998-12-31"));
        assert_eq!(date("1,January,1999").as_deref(), Some("1999-01-01"));
        assert_eq!(date("29,February,2000").as_deref(), Some("2000-02-29"));
        for bad in [
            "29,February,1900",
            "31,April,2004",
            "0,May,2004",
            "01,may,2004",
            "1,May,04",
            " 1,May,2004",
            "1,May,2004,extra",
            "1,January,0000",
        ] {
            assert!(date(bad).is_none(), "{bad}");
        }
        let bytes = b"<date>31,December,1998</date>\n<post>\nOld.\n</post>\n<date>1,January,1999</date>\n<post>\nNew.\n</post>\n";
        let posts = parse(bytes);
        assert_eq!(posts[0].rejection, None);
        assert_eq!(posts[1].rejection, None);
        let legacy = parse_with_date_policy(bytes, DatePolicy::Legacy1999);
        assert_eq!(legacy[0].rejection, Some("before_1999"));
        assert_eq!(legacy[1].rejection, None);
    }

    #[test]
    fn preserves_punctuation_unicode_and_exact_byte_spans() {
        let bytes = "<Blog>\r\n<date>2,May,2004</date>\r\n<post>\r\n \tCafé says: ‘wait!’\r\nNext line. \r\n</post>\r\n</Blog>".as_bytes();
        let post = parse(bytes).remove(0);
        let value = text(bytes, post.body, Encoding::Utf8).unwrap();
        assert_eq!(value.exact, "Café says: ‘wait!’\r\nNext line.");
        assert_eq!(&bytes[value.trimmed_span], value.exact.as_bytes());
        assert_eq!(
            value.normalized_sha256,
            hash("Café says: ‘wait!’ Next line.".as_bytes())
        );
    }

    #[test]
    fn malformed_boundaries_never_join_neighboring_posts() {
        let bytes = b"<post>\nUndated\n</post>\n<date>3,May,2004</date>\n<post>\nUnclosed\n<date>bad</date>\n<post>\nUnknown\n</post>\n<date>4,May,2004</date>\n<post>\nKept\n</post>\n<post>\nSame date\n</post>\n</post>\n";
        let posts = parse(bytes);
        assert_eq!(posts.len(), 6);
        assert_eq!(posts[0].rejection, Some("missing_date"));
        assert_eq!(posts[1].rejection, Some("unclosed_post"));
        assert_eq!(posts[2].rejection, Some("malformed_date"));
        assert_eq!(posts[3].rejection, None);
        assert_eq!(posts[4].date.as_deref(), Some("2004-05-04"));
        assert_eq!(posts[5].rejection, Some("orphan_post_close"));
        assert_eq!(
            text(bytes, posts[3].body.clone(), Encoding::Latin1)
                .unwrap()
                .exact,
            "Kept"
        );
    }

    #[test]
    fn no_lossy_decoding_or_encoding_guessing() {
        assert!(encoding(b"<?xml version='1.0' encoding='Windows-1252'?>").is_none());
        assert!(matches!(
            encoding(b"<?xml version='1.0' encoding='UTF-8'?>"),
            Some(Encoding::Utf8)
        ));
        assert!(matches!(encoding(b"<Blog>"), Some(Encoding::Latin1)));
        assert_eq!(
            text(b"bad\xff", 0..4, Encoding::Utf8).unwrap_err(),
            "invalid_declared_utf8"
        );
        let raw = b" \xa0caf\xe9\x92 \n";
        let value = text(raw, 0..raw.len(), Encoding::Latin1).unwrap();
        assert_eq!(value.exact, "café\u{92}");
        assert_eq!(&raw[value.trimmed_span], b"caf\xe9\x92");
        assert!(value.has_c1_controls);
        let reconstructed: Vec<_> = value.exact.chars().map(|ch| ch as u8).collect();
        assert_eq!(reconstructed, b"caf\xe9\x92");
        assert!(
            text(b"Text.\x85", 0..6, Encoding::Latin1)
                .unwrap()
                .has_c1_controls
        );
    }

    #[test]
    fn validates_numeric_author_ids_and_rejects_archive_path_tricks() {
        assert_eq!(author_id("blogs/123.female.25.indUnk.Leo.xml"), Some("123"));
        for name in [
            "../123.female.25.indUnk.Leo.xml",
            "/123.female.25.indUnk.Leo.xml",
            "blogs\\123.female.25.indUnk.Leo.xml",
            "blogs/../../123.female.25.indUnk.Leo.xml",
            "blogs/C:/123.female.25.indUnk.Leo.xml",
            "blogs/nonnumeric.female.25.indUnk.Leo.xml",
        ] {
            assert!(author_id(name).is_none(), "{name}");
        }
    }
}

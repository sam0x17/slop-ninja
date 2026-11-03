//! Lossless line skeletons.
//!
//! A JSON line is split into raw byte segments and long string values. Each
//! long string value is stored once in the content-addressed blob store and
//! replaced by its SHA-256 and the escaping style that reproduces the original
//! literal byte for byte. A value whose literal no known style reproduces stays
//! raw, so reconstruction is always exact.

use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};

/// String values whose literal is at least this long (bytes, quotes included)
/// are moved to the blob store.
pub const MIN_BLOB_LITERAL: usize = 160;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Escape `"`, `\`, and U+0000..U+001F (short forms for \b \f \n \r \t); everything else raw
    /// (serde_json, Python `ensure_ascii=False`).
    Minimal = 1,
    /// As Minimal, plus every code point outside U+0020..U+007E as lowercase `\uXXXX`
    /// with surrogate pairs (Python `json.dumps` default).
    Ascii = 2,
    /// As Minimal, plus U+007F as `\u007f` (jq).
    MinimalDel = 3,
}

impl Style {
    pub fn from_u8(b: u8) -> Result<Style> {
        Ok(match b {
            1 => Style::Minimal,
            2 => Style::Ascii,
            3 => Style::MinimalDel,
            _ => bail!("unknown escaping style {b}"),
        })
    }
}

pub const STYLES: [Style; 3] = [Style::Minimal, Style::Ascii, Style::MinimalDel];

/// Encode `s` as a JSON string literal (with quotes) in `style`.
pub fn encode(s: &str, style: Style) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    out.push(b'"');
    let mut buf = [0u16; 2];
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            c if (c as u32) < 0x20 => out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes()),
            '\u{7f}' if style != Style::Minimal => out.extend_from_slice(b"\\u007f"),
            c if style == Style::Ascii && (c as u32) > 0x7e => {
                for u in c.encode_utf16(&mut buf) {
                    out.extend_from_slice(format!("\\u{:04x}", u).as_bytes());
                }
            }
            c => {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
    }
    out.push(b'"');
    out
}

/// A string literal found in a JSON line: byte range including quotes, and
/// whether it is an object key.
#[derive(Debug, Clone, Copy)]
pub struct Literal {
    pub start: usize,
    pub end: usize,
    pub is_key: bool,
}

/// Every string literal of one JSON value, in order. Fails on an unterminated string.
pub fn literals(line: &[u8]) -> Result<Vec<Literal>> {
    let mut out = Vec::new();
    let mut i = 0;
    let n = line.len();
    while i < n {
        if line[i] == b'"' {
            let start = i;
            i += 1;
            loop {
                ensure!(i < n, "unterminated string literal at byte {start}");
                match line[i] {
                    b'\\' => i += 2,
                    b'"' => break,
                    _ => i += 1,
                }
            }
            let end = i + 1;
            let mut j = end;
            while j < n && matches!(line[j], b' ' | b'\t' | b'\r' | b'\n') {
                j += 1;
            }
            out.push(Literal { start, end, is_key: j < n && line[j] == b':' });
            i = end;
        } else {
            i += 1;
        }
    }
    Ok(out)
}

/// One segment of a skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Raw(Vec<u8>),
    Blob { sha: [u8; 32], style: Style },
}

/// A long string value moved to the blob store.
pub struct Extracted {
    pub sha: [u8; 32],
    pub text: String,
    /// Byte range of the literal in the line.
    pub start: usize,
    pub end: usize,
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Split a line into segments. Returns the segments and the extracted strings.
pub fn split(line: &[u8]) -> Result<(Vec<Seg>, Vec<Extracted>)> {
    let mut segs = Vec::new();
    let mut ext = Vec::new();
    let mut pos = 0;
    for lit in literals(line)? {
        if lit.is_key || lit.end - lit.start < MIN_BLOB_LITERAL {
            continue;
        }
        let raw = &line[lit.start..lit.end];
        let Ok(text) = serde_json::from_slice::<String>(raw) else { continue };
        let Some(style) = STYLES.into_iter().find(|s| encode(&text, *s) == raw) else { continue };
        if lit.start > pos {
            segs.push(Seg::Raw(line[pos..lit.start].to_vec()));
        }
        let sha = sha256(text.as_bytes());
        segs.push(Seg::Blob { sha, style });
        ext.push(Extracted { sha, text, start: lit.start, end: lit.end });
        pos = lit.end;
    }
    if pos < line.len() || segs.is_empty() {
        segs.push(Seg::Raw(line[pos..].to_vec()));
    }
    Ok((segs, ext))
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        ensure!(*pos < buf.len(), "truncated varint");
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        ensure!(shift < 64, "varint overflow");
    }
}

/// Serialise one line's segments: varint count, then per segment tag 0 + varint
/// length + bytes, or tag = style (1..3) + 32-byte SHA-256.
pub fn write_line(out: &mut Vec<u8>, segs: &[Seg]) {
    put_varint(out, segs.len() as u64);
    for s in segs {
        match s {
            Seg::Raw(b) => {
                out.push(0);
                put_varint(out, b.len() as u64);
                out.extend_from_slice(b);
            }
            Seg::Blob { sha, style } => {
                out.push(*style as u8);
                out.extend_from_slice(sha);
            }
        }
    }
}

pub fn read_line(buf: &[u8], pos: &mut usize) -> Result<Vec<Seg>> {
    let n = get_varint(buf, pos)? as usize;
    let mut segs = Vec::with_capacity(n);
    for _ in 0..n {
        ensure!(*pos < buf.len(), "truncated segment");
        let tag = buf[*pos];
        *pos += 1;
        if tag == 0 {
            let len = get_varint(buf, pos)? as usize;
            ensure!(*pos + len <= buf.len(), "truncated raw segment");
            segs.push(Seg::Raw(buf[*pos..*pos + len].to_vec()));
            *pos += len;
        } else {
            ensure!(*pos + 32 <= buf.len(), "truncated blob segment");
            let mut sha = [0u8; 32];
            sha.copy_from_slice(&buf[*pos..*pos + 32]);
            *pos += 32;
            segs.push(Seg::Blob { sha, style: Style::from_u8(tag)? });
        }
    }
    Ok(segs)
}

/// Rebuild a line from its segments.
pub fn rebuild(segs: &[Seg], mut fetch: impl FnMut(&[u8; 32]) -> Result<String>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for s in segs {
        match s {
            Seg::Raw(b) => out.extend_from_slice(b),
            Seg::Blob { sha, style } => out.extend_from_slice(&encode(&fetch(sha)?, *style)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn roundtrip(line: &[u8]) -> usize {
        let (segs, ext) = split(line).unwrap();
        let map: HashMap<[u8; 32], String> = ext.iter().map(|e| (e.sha, e.text.clone())).collect();
        let mut buf = Vec::new();
        write_line(&mut buf, &segs);
        let mut pos = 0;
        let back = read_line(&buf, &mut pos).unwrap();
        assert_eq!(pos, buf.len());
        let out = rebuild(&back, |s| Ok(map[s].clone())).unwrap();
        assert_eq!(out, line);
        ext.len()
    }

    #[test]
    fn styles_roundtrip_exactly() {
        let long = "Caf\u{e9} \u{1F600} \"quoted\" back\\slash\nnew\ttab \u{7f} ".repeat(20);
        let minimal = serde_json::to_vec(&serde_json::json!({"id":"a","text":long,"n":1})).unwrap();
        assert_eq!(roundtrip(&minimal), 1);
        let ascii = format!("{{\"text\": {}, \"k\": \"v\"}}", String::from_utf8(encode(&long, Style::Ascii)).unwrap());
        assert_eq!(roundtrip(ascii.as_bytes()), 1);
        let del = format!("{{\"text\":{}}}", String::from_utf8(encode(&long, Style::MinimalDel)).unwrap());
        assert_eq!(roundtrip(del.as_bytes()), 1);
    }

    #[test]
    fn unusual_escapes_stay_raw() {
        // `\/` and uppercase hex are valid JSON that no style reproduces.
        let body = "a\\/b\\u00E9".repeat(40);
        let line = format!("{{\"text\":\"{body}\"}}");
        assert_eq!(roundtrip(line.as_bytes()), 0);
    }

    #[test]
    fn keys_and_short_values_stay_raw() {
        let key = "k".repeat(300);
        let line = format!("{{\"{key}\":\"short\",\"t\":\"{}\"}}", "x".repeat(200));
        assert_eq!(roundtrip(line.as_bytes()), 1);
        assert_eq!(roundtrip(b"{}"), 0);
        assert_eq!(roundtrip(b""), 0);
    }
}

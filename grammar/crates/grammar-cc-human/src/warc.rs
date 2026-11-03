//! WARC/1.0 records from a Common Crawl WET or WARC file.
//!
//! Common Crawl writes every record as its own gzip member, so a record's
//! (file, member offset, member length) is a stable pointer: an HTTP Range
//! read of those bytes decodes to exactly that record after the raw file is
//! deleted. `Members` walks the members of a whole file held in memory and
//! reports each member's compressed offset and length. `WARC-Block-Digest`
//! (`sha1:` + RFC 4648 base32) is checked against the block bytes.
use anyhow::{Context, Result, bail, ensure};
use flate2::bufread::GzDecoder;
use sha1::{Digest, Sha1};
use std::io::Read;

pub struct Record {
    /// Compressed offset of the record's gzip member in the file.
    pub offset: u64,
    /// Compressed length of the gzip member.
    pub length: u64,
    pub headers: Vec<(String, String)>,
    pub block: Vec<u8>,
}

impl Record {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    pub fn kind(&self) -> &str {
        self.header("WARC-Type").unwrap_or("")
    }
    /// `Some(true)` when the declared block digest matches, `Some(false)` on
    /// a mismatch, `None` when there is no SHA-1 block digest.
    pub fn block_digest_ok(&self) -> Option<bool> {
        let d = self.header("WARC-Block-Digest")?;
        let b32 = d.strip_prefix("sha1:")?;
        Some(base32(&Sha1::digest(&self.block)) == b32.trim().to_ascii_uppercase())
    }
}

/// RFC 4648 base32 (upper case, no padding needed for 20-byte digests).
pub fn base32(b: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(b.len().div_ceil(5) * 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &x in b {
        acc = (acc << 8) | x as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((acc << (5 - bits)) & 31) as usize] as char);
    }
    while !out.len().is_multiple_of(8) {
        out.push('=');
    }
    out
}

/// Parse one decoded WARC record (`WARC/1.x` header block, CRLF CRLF,
/// `Content-Length` bytes of block).
pub fn parse_record(raw: &[u8], offset: u64, length: u64) -> Result<Record> {
    let end = find(raw, b"\r\n\r\n").context("WARC header terminator")?;
    let head = std::str::from_utf8(&raw[..end]).context("WARC header is not UTF-8")?;
    let mut lines = head.split("\r\n");
    let version = lines.next().unwrap_or("");
    ensure!(version.starts_with("WARC/1."), "not a WARC record: {version:?}");
    let mut headers = Vec::new();
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            bail!("bad WARC header line {l:?}")
        };
        headers.push((k.trim().to_owned(), v.trim().to_owned()));
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
        .context("Content-Length")?
        .1
        .parse()
        .context("Content-Length value")?;
    let start = end + 4;
    ensure!(
        raw.len() >= start + len,
        "record shorter than Content-Length ({} < {})",
        raw.len() - start,
        len
    );
    Ok(Record {
        offset,
        length,
        headers,
        block: raw[start..start + len].to_vec(),
    })
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Iterator over the gzip members of a file held in memory.
pub struct Members<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Members<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Members { data, pos: 0 }
    }
}

/// Decode the single gzip member at the start of `data`; returns the
/// decoded bytes and the member's compressed length.
pub fn decode_member(data: &[u8]) -> Result<(Vec<u8>, usize)> {
    let mut rest = data;
    let mut out = Vec::new();
    {
        let mut d = GzDecoder::new(&mut rest);
        d.read_to_end(&mut out).context("gzip member")?;
    }
    let used = data.len() - rest.len();
    ensure!(used > 0, "empty gzip member");
    Ok((out, used))
}

impl Iterator for Members<'_> {
    type Item = Result<Record>;
    fn next(&mut self) -> Option<Result<Record>> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        match decode_member(&self.data[start..]) {
            Ok((raw, used)) => {
                self.pos += used;
                Some(parse_record(&raw, start as u64, used as u64).with_context(|| format!("record at offset {start}")))
            }
            Err(e) => {
                self.pos = self.data.len();
                Some(Err(e.context(format!("gzip member at offset {start}"))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    fn member(s: &[u8]) -> Vec<u8> {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(s).unwrap();
        e.finish().unwrap()
    }

    fn rec(uri: &str, body: &str) -> Vec<u8> {
        let d = base32(&Sha1::digest(body.as_bytes()));
        format!(
            "WARC/1.0\r\nWARC-Type: conversion\r\nWARC-Target-URI: {uri}\r\nWARC-Date: 2019-01-02T03:04:05Z\r\nWARC-Block-Digest: sha1:{d}\r\nContent-Length: {}\r\n\r\n{body}\r\n\r\n",
            body.len()
        )
        .into_bytes()
    }

    #[test]
    fn base32_matches_rfc4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY======");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI======");
        assert_eq!(base32(&[0u8; 20]).len(), 32);
    }

    #[test]
    fn members_report_offsets_that_decode_alone() {
        let a = member(&rec("http://a.example/1", "first body\nline two"));
        let b = member(&rec("http://b.example/2", "second"));
        let mut file = a.clone();
        file.extend_from_slice(&b);
        let recs: Vec<Record> = Members::new(&file).collect::<Result<_>>().unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!((recs[0].offset, recs[0].length), (0, a.len() as u64));
        assert_eq!((recs[1].offset, recs[1].length), (a.len() as u64, b.len() as u64));
        assert_eq!(recs[1].header("warc-target-uri"), Some("http://b.example/2"));
        assert_eq!(recs[0].block, b"first body\nline two");
        assert_eq!(recs[0].block_digest_ok(), Some(true));
        let (raw, used) = decode_member(&file[recs[1].offset as usize..]).unwrap();
        assert_eq!(used as u64, recs[1].length);
        assert_eq!(parse_record(&raw, 0, 0).unwrap().block, b"second");
    }

    #[test]
    fn digest_mismatch_is_reported() {
        let mut r = parse_record(&rec("http://a.example/", "abc"), 0, 0).unwrap();
        r.block = b"abd".to_vec();
        assert_eq!(r.block_digest_ok(), Some(false));
    }
}

//! Exact file archiving. A file is identified by the SHA-256 of its (decoded)
//! bytes. JSONL files are archived line by line as skeletons whose long string
//! values live in the blob store; other files are stored whole. Either way the
//! file can be rebuilt byte for byte (`rebuild_file`).

use crate::packs::{self, Packs};
use crate::sink::{At, Sink};
use crate::skeleton::{self, Seg};
use anyhow::{Context, Result, bail, ensure};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

pub const CHUNK_BYTES: usize = 32 << 20;
pub const BATCH_LINES: usize = 4096;
pub const WHOLE_LIMIT: u64 = 512 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jsonl,
    Json,
    Raw,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Jsonl => "jsonl",
            Format::Json => "json",
            Format::Raw => "raw",
        }
    }
}

/// Open a file, transparently decoding `.zst`.
pub fn reader(path: &Path) -> Result<Box<dyn Read + Send>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if path.extension().is_some_and(|e| e == "zst") {
        Ok(Box::new(zstd::stream::read::Decoder::new(f)?))
    } else {
        Ok(Box::new(f))
    }
}

/// SHA-256 and length of the decoded bytes.
pub fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut r = reader(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((hex::encode(h.finalize()), n))
}

/// SHA-256 of a file's stored bytes (no decoding).
pub fn hash_file_raw(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(hex::encode(h.finalize()))
}

/// One parsed line handed to an adapter.
pub struct Line<'a> {
    pub at: At,
    pub bytes: &'a [u8],
    pub value: Option<&'a Value>,
}

pub fn file_complete(db: &Connection, sha: &str) -> Result<bool> {
    Ok(db
        .prepare_cached("SELECT complete FROM file WHERE file_sha256 = ?1")?
        .query_row([sha], |r| r.get::<_, i64>(0))
        .optional()?
        == Some(1))
}

struct Prepared {
    line_sha: String,
    segs: Vec<Seg>,
    ext: Vec<skeleton::Extracted>,
    value: Option<Value>,
}

/// Archive `path` (decoded SHA-256 `sha`) and call `each` for every line (JSONL),
/// or once for the whole file (JSON/raw; `value` is the parsed JSON when it parses).
/// When the file is already complete in the store, only the callbacks run.
/// Runs inside the batch transaction of the sink, which it may checkpoint.
pub fn ingest_file(
    sink: &mut Sink,
    dir: &Path,
    path: &Path,
    rel: &str,
    sha: &str,
    n_bytes: u64,
    fmt: Format,
    each: &mut dyn FnMut(&mut Sink, &Line) -> Result<()>,
) -> Result<()> {
    let already = file_complete(sink.db, sha)?;
    {
        sink.db
            .prepare_cached("INSERT OR IGNORE INTO file_path(path, file_sha256, source_id) VALUES (?1, ?2, ?3)")?
            .execute(params![rel, sha, sink.source_id])?;
        if !already {
            sink.db.execute("DELETE FROM file_chunk WHERE file_sha256 = ?1", [sha])?;
            sink.db.execute(
                "INSERT OR REPLACE INTO file(file_sha256, n_bytes, n_lines, format, whole_blob, complete, final_newline) VALUES (?1, ?2, NULL, ?3, ?4, 0, 1)",
                params![sha, n_bytes as i64, fmt.name(), (fmt != Format::Jsonl) as i64],
            )?;
        }
    }
    match fmt {
        Format::Jsonl => ingest_jsonl(sink, dir, path, rel, sha, already, each),
        Format::Json | Format::Raw => {
            ensure!(n_bytes <= WHOLE_LIMIT, "{} is too large to store whole ({n_bytes} bytes)", path.display());
            let mut bytes = Vec::with_capacity(n_bytes as usize);
            reader(path)?.read_to_end(&mut bytes)?;
            ensure!(hex::encode(skeleton::sha256(&bytes)) == sha, "{} changed while ingesting", path.display());
            {
                if !already {
                    let s = skeleton::sha256(&bytes);
                    sink.packs.put(sink.db, &s, &bytes, None)?;
                }
                let value = if fmt == Format::Json { serde_json::from_slice::<Value>(&bytes).ok() } else { None };
                let at = At { file_sha256: sha.into(), line_no: 0, line_sha256: sha.into(), path: rel.into() };
                if fmt == Format::Json && value.is_none() {
                    sink.problem("json_parse", Some(rel), None, "file is not valid JSON")?;
                }
                each(sink, &Line { at, bytes: &bytes, value: value.as_ref() })?;
                if !already {
                    sink.db.execute("UPDATE file SET n_lines = 1, complete = 1 WHERE file_sha256 = ?1", [sha])?;
                }
            }
            sink.work(1);
            Ok(())
        }
    }
}

fn ingest_jsonl(
    sink: &mut Sink,
    dir: &Path,
    path: &Path,
    rel: &str,
    sha: &str,
    already: bool,
    each: &mut dyn FnMut(&mut Sink, &Line) -> Result<()>,
) -> Result<()> {
    let _ = dir;
    let mut rd = BufReader::with_capacity(8 << 20, reader(path)?);
    let mut hasher = Sha256::new();
    let mut line_no: i64 = 0;
    let mut chunk_no: i64 = 0;
    let mut chunk = Vec::new();
    let mut chunk_first: i64 = 0;
    let mut chunk_lines: i64 = 0;
    let mut final_newline = true;
    let mut eof = false;
    while !eof {
        let mut lines: Vec<Vec<u8>> = Vec::with_capacity(BATCH_LINES);
        let mut batch_bytes = 0usize;
        while lines.len() < BATCH_LINES && batch_bytes < CHUNK_BYTES {
            let mut buf = Vec::new();
            let k = rd.read_until(b'\n', &mut buf)?;
            if k == 0 {
                eof = true;
                break;
            }
            hasher.update(&buf);
            if buf.last() == Some(&b'\n') {
                buf.pop();
            } else {
                final_newline = false;
                eof = true;
            }
            batch_bytes += buf.len();
            lines.push(buf);
        }
        if lines.is_empty() {
            break;
        }
        let prepared: Vec<Prepared> = lines
            .par_iter()
            .map(|l| {
                let line_sha = hex::encode(skeleton::sha256(l));
                let (segs, ext) = if already {
                    (Vec::new(), Vec::new())
                } else {
                    skeleton::split(l).unwrap_or_else(|_| (vec![Seg::Raw(l.clone())], Vec::new()))
                };
                let value = serde_json::from_slice::<Value>(l).ok();
                Prepared { line_sha, segs, ext, value }
            })
            .collect();
        {
            if !already {
                // Compress only blobs not yet stored, in parallel.
                let mut missing: Vec<(usize, usize)> = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for (i, p) in prepared.iter().enumerate() {
                    for (j, e) in p.ext.iter().enumerate() {
                        if seen.insert(e.sha) && !Packs::has(sink.db, &e.sha)? {
                            missing.push((i, j));
                        }
                    }
                }
                let frames: Vec<Vec<u8>> = missing
                    .par_iter()
                    .map(|&(i, j)| zstd::bulk::compress(prepared[i].ext[j].text.as_bytes(), packs::LEVEL).expect("zstd"))
                    .collect();
                for (&(i, j), z) in missing.iter().zip(frames) {
                    let e = &prepared[i].ext[j];
                    sink.packs.put(sink.db, &e.sha, e.text.as_bytes(), Some(z))?;
                }
            }
            for (k, p) in prepared.iter().enumerate() {
                let ln = line_no + k as i64;
                if !already {
                    if chunk_lines == 0 {
                        chunk_first = ln;
                    }
                    skeleton::write_line(&mut chunk, &p.segs);
                    chunk_lines += 1;
                }
                let at = At { file_sha256: sha.into(), line_no: ln, line_sha256: p.line_sha.clone(), path: rel.into() };
                if p.value.is_none() && !lines[k].is_empty() {
                    sink.problem("jsonl_parse", Some(rel), Some(ln), "line is not valid JSON")?;
                }
                each(sink, &Line { at, bytes: &lines[k], value: p.value.as_ref() })?;
            }
            if !already && (chunk.len() >= CHUNK_BYTES || eof) && chunk_lines > 0 {
                let s = skeleton::sha256(&chunk);
                sink.packs.put(sink.db, &s, &chunk, None)?;
                sink.db.execute(
                    "INSERT OR REPLACE INTO file_chunk(file_sha256, chunk_no, first_line, n_lines, blob_sha256) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![sha, chunk_no, chunk_first, chunk_lines, &s[..]],
                )?;
                chunk_no += 1;
                chunk.clear();
                chunk_lines = 0;
            }
        }
        line_no += lines.len() as i64;
        sink.work(lines.len() as u64 / 8 + 1);
        sink.checkpoint(false, false)?;
    }
    let got = hex::encode(hasher.finalize());
    if got != sha {
        bail!("{} changed while ingesting (hash {got}, expected {sha})", path.display());
    }
    {
        if !already {
            if chunk_lines > 0 {
                let s = skeleton::sha256(&chunk);
                sink.packs.put(sink.db, &s, &chunk, None)?;
                sink.db.execute(
                    "INSERT OR REPLACE INTO file_chunk(file_sha256, chunk_no, first_line, n_lines, blob_sha256) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![sha, chunk_no, chunk_first, chunk_lines, &s[..]],
                )?;
            }
            sink.db.execute(
                "UPDATE file SET n_lines = ?2, complete = 1, final_newline = ?3 WHERE file_sha256 = ?1",
                params![sha, line_no, final_newline as i64],
            )?;
        }
    }
    sink.work(1);
    Ok(())
}

/// Rebuild a stored file byte for byte into `out`; returns its SHA-256.
pub fn rebuild_file(db: &Connection, dir: &Path, sha: &str, out: &mut dyn Write) -> Result<String> {
    let (fmt, whole, n_lines, final_nl): (String, i64, Option<i64>, i64) = db
        .query_row("SELECT format, whole_blob, n_lines, final_newline FROM file WHERE file_sha256 = ?1 AND complete = 1", [sha], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .with_context(|| format!("file {sha} is not complete in the store"))?;
    let mut h = Sha256::new();
    if whole == 1 {
        let mut s = [0u8; 32];
        hex::decode_to_slice(sha, &mut s)?;
        let b = packs::get(db, dir, &s)?;
        h.update(&b);
        out.write_all(&b)?;
        return Ok(hex::encode(h.finalize()));
    }
    ensure!(fmt == "jsonl", "unexpected format {fmt}");
    let mut st = db.prepare("SELECT blob_sha256, n_lines FROM file_chunk WHERE file_sha256 = ?1 ORDER BY chunk_no")?;
    let chunks: Vec<(Vec<u8>, i64)> = st.query_map([sha], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut cache = BlobCache::new(dir);
    let mut written = 0i64;
    let total = n_lines.unwrap_or(0);
    for (c, n) in chunks {
        let mut cs = [0u8; 32];
        cs.copy_from_slice(&c);
        let buf = packs::get(db, dir, &cs)?;
        let mut pos = 0;
        for _ in 0..n {
            let segs = skeleton::read_line(&buf, &mut pos)?;
            let line = skeleton::rebuild(&segs, |s| cache.text(db, s))?;
            h.update(&line);
            out.write_all(&line)?;
            written += 1;
            if written < total || final_nl == 1 {
                h.update(b"\n");
                out.write_all(b"\n")?;
            }
        }
    }
    ensure!(written == total, "rebuilt {written} lines, expected {total}");
    Ok(hex::encode(h.finalize()))
}

/// Rebuild selected lines of a stored file (line numbers from 0), calling
/// `each(line_no, bytes, whole_file)` in line order. A JSONL file is read
/// chunk by chunk, so each skeleton chunk is decoded once; a file stored whole
/// yields its complete bytes once, as line 0. The caller checks each line
/// against its recorded digest.
pub fn rebuild_lines(
    db: &Connection,
    cache: &mut BlobCache,
    sha: &str,
    want: &std::collections::BTreeSet<i64>,
    each: &mut dyn FnMut(i64, &[u8], bool) -> Result<()>,
) -> Result<()> {
    if want.is_empty() {
        return Ok(());
    }
    let (whole, n_lines): (i64, Option<i64>) = db
        .prepare_cached("SELECT whole_blob, n_lines FROM file WHERE file_sha256 = ?1 AND complete = 1")?
        .query_row([sha], |r| Ok((r.get(0)?, r.get(1)?)))
        .with_context(|| format!("file {sha} is not complete in the store"))?;
    if whole == 1 {
        if want.contains(&0) {
            let mut s = [0u8; 32];
            hex::decode_to_slice(sha, &mut s)?;
            let b = cache.bytes(db, &s)?;
            each(0, &b, true)?;
        }
        return Ok(());
    }
    let last = *want.iter().next_back().unwrap();
    ensure!(last < n_lines.unwrap_or(0), "line {last} beyond the {} lines of {sha}", n_lines.unwrap_or(0));
    let chunks: Vec<(i64, i64, Vec<u8>)> = db
        .prepare_cached("SELECT first_line, n_lines, blob_sha256 FROM file_chunk WHERE file_sha256 = ?1 ORDER BY chunk_no")?
        .query_map([sha], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (first, n, blob) in chunks {
        let Some(&lo) = want.range(first..first + n).next() else { continue };
        let hi = *want.range(first..first + n).next_back().unwrap();
        let mut cs = [0u8; 32];
        cs.copy_from_slice(&blob);
        let buf = cache.bytes(db, &cs)?;
        let mut pos = 0;
        for ln in first..=hi {
            let segs = skeleton::read_line(&buf, &mut pos)?;
            if ln >= lo && want.contains(&ln) {
                let line = skeleton::rebuild(&segs, |s| cache.text(db, s))?;
                each(ln, &line, false)?;
            }
        }
    }
    Ok(())
}

/// Rebuild one line of a stored file and check it against `line_sha256`.
#[cfg(test)]
pub fn rebuild_line(db: &Connection, dir: &Path, sha: &str, line_no: i64, line_sha256: &str) -> Result<Vec<u8>> {
    let mut cache = BlobCache::new(dir);
    let mut out = None;
    let want = std::collections::BTreeSet::from([line_no]);
    rebuild_lines(db, &mut cache, sha, &want, &mut |_, b, _| {
        out = Some(b.to_vec());
        Ok(())
    })?;
    let b = out.with_context(|| format!("line {line_no} of {sha} not found"))?;
    ensure!(hex::encode(skeleton::sha256(&b)) == line_sha256, "line {line_no} of {sha} differs from its recorded digest");
    Ok(b)
}

/// Small cache with open pack handles for rebuilding many lines.
pub struct BlobCache {
    dir: std::path::PathBuf,
    files: std::collections::HashMap<i64, File>,
}

impl BlobCache {
    pub fn new(dir: &Path) -> BlobCache {
        BlobCache { dir: dir.to_path_buf(), files: Default::default() }
    }
    pub fn bytes(&mut self, db: &Connection, sha: &[u8; 32]) -> Result<Vec<u8>> {
        use std::io::{Seek, SeekFrom};
        let (pack, off, zlen, len): (i64, i64, i64, i64) = db
            .prepare_cached("SELECT pack_id, offset, zlen, len FROM blob WHERE sha256 = ?1")?
            .query_row([&sha[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .with_context(|| format!("blob {} not in store", hex::encode(sha)))?;
        if !self.files.contains_key(&pack) {
            self.files.insert(pack, File::open(packs::pack_path(&self.dir, pack))?);
        }
        let f = self.files.get_mut(&pack).unwrap();
        f.seek(SeekFrom::Start(off as u64))?;
        let mut z = vec![0u8; zlen as usize];
        f.read_exact(&mut z)?;
        let raw = zstd::bulk::decompress(&z, len as usize)?;
        ensure!(skeleton::sha256(&raw) == *sha, "blob {} fails its digest", hex::encode(sha));
        Ok(raw)
    }
    pub fn text(&mut self, db: &Connection, sha: &[u8; 32]) -> Result<String> {
        Ok(String::from_utf8(self.bytes(db, sha)?)?)
    }
}

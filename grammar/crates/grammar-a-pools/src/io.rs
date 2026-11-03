//! Immutable outputs, hashed inputs and transparent zstd reading.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

/// Hashes every byte read through it, so a streamed dump is bound to the
/// exact (decompressed) bytes the adapter consumed.
pub struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    pub bytes: u64,
}

impl<R: Read> HashingReader<R> {
    pub fn new(inner: R) -> Self {
        HashingReader {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }
    pub fn finish(self) -> (String, u64) {
        (hex::encode(self.hasher.finalize()), self.bytes)
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// Open a path (`-` is stdin); `.zst` and `.gz` files are decoded transparently.
pub fn open(path: &Path) -> Result<Box<dyn Read>> {
    if path == Path::new("-") {
        return Ok(Box::new(io::stdin().lock()));
    }
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if path.extension().is_some_and(|e| e == "zst") {
        Ok(Box::new(zstd::Decoder::new(f)?))
    } else if path.extension().is_some_and(|e| e == "gz") {
        Ok(Box::new(flate2::read::MultiGzDecoder::new(BufReader::with_capacity(1 << 20, f))))
    } else {
        Ok(Box::new(f))
    }
}

pub fn read_all(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    open(path)?.take(limit + 1).read_to_end(&mut b)?;
    ensure!(b.len() as u64 <= limit, "input exceeds {limit} bytes: {}", path.display());
    Ok(b)
}

pub fn file_sha(path: &Path) -> Result<(String, u64)> {
    let mut r = HashingReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    io::copy(&mut r, &mut io::sink())?;
    Ok(r.finish())
}

/// Create an output directory that must not already hold files.
pub fn fresh_dir(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(
            fs::read_dir(path)?.next().is_none(),
            "output directory is not empty (use a successor path): {}",
            path.display()
        );
    } else {
        fs::create_dir_all(path)?;
    }
    Ok(())
}

/// Append-only JSONL writer that refuses to replace an existing file and
/// hashes what it writes.
pub struct JsonlWriter {
    out: BufWriter<File>,
    hasher: Sha256,
    pub rows: u64,
    pub path: PathBuf,
}

impl JsonlWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        Ok(JsonlWriter {
            out: BufWriter::new(f),
            hasher: Sha256::new(),
            rows: 0,
            path: path.to_owned(),
        })
    }
    pub fn write(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        self.hasher.update(&line);
        self.out.write_all(&line)?;
        self.rows += 1;
        Ok(())
    }
    /// Flush, fsync and return (sha256, rows).
    pub fn finish(self) -> Result<(String, u64)> {
        let f = self.out.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        Ok((hex::encode(self.hasher.finalize()), self.rows))
    }
}

pub fn write_json(path: &Path, v: &Value) -> Result<String> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(&b)?;
    f.sync_all()?;
    Ok(crate::text::sha_hex(&b))
}

/// Iterate JSONL rows of a (possibly zstd) file.
pub fn jsonl_rows(path: &Path) -> Result<impl Iterator<Item = Result<Value>>> {
    let r = BufReader::with_capacity(1 << 20, open(path)?);
    let p = path.display().to_string();
    Ok(r.lines().enumerate().filter_map(move |(i, l)| match l {
        Ok(l) if l.trim().is_empty() => None,
        Ok(l) => Some(serde_json::from_str(&l).with_context(|| format!("{p}:{}", i + 1))),
        Err(e) => Some(Err(anyhow::Error::from(e).context(format!("{p}:{}", i + 1)))),
    }))
}

pub fn now_utc() -> String {
    // Seconds since the epoch rendered as RFC 3339 without a date crate.
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// RFC 3339 UTC timestamp of seconds since the epoch.
pub fn utc_from_unix(s: i64) -> String {
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writer_is_create_new_and_hashes_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.jsonl");
        let mut w = JsonlWriter::create(&p).unwrap();
        w.write(&serde_json::json!({"a":1})).unwrap();
        let (h, n) = w.finish().unwrap();
        assert_eq!((h, n), (crate::text::sha_hex(b"{\"a\":1}\n"), 1));
        assert!(JsonlWriter::create(&p).is_err());
        let rows: Vec<_> = jsonl_rows(&p).unwrap().collect::<Result<_>>().unwrap();
        assert_eq!(rows[0]["a"], 1);
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_326), (2022, 11, 30));
        assert_eq!(utc_from_unix(1_669_766_399), "2022-11-29T23:59:59Z");
    }
}

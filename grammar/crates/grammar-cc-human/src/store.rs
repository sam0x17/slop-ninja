//! Atomic zstd JSONL and JSON outputs (written to `.tmp`, fsynced, renamed).
use anyhow::{Context, Result};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

pub struct ZWriter {
    enc: zstd::stream::write::Encoder<'static, BufWriter<File>>,
    h: Sha256,
    pub rows: u64,
    tmp: PathBuf,
    path: PathBuf,
}

fn tmp_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

impl ZWriter {
    pub fn create(path: &Path) -> Result<ZWriter> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let tmp = tmp_of(path);
        let f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        Ok(ZWriter {
            enc: zstd::stream::write::Encoder::new(BufWriter::new(f), 6)?,
            h: Sha256::new(),
            rows: 0,
            tmp,
            path: path.to_owned(),
        })
    }
    pub fn write<T: Serialize>(&mut self, v: &T) -> Result<()> {
        let mut b = serde_json::to_vec(v)?;
        b.push(b'\n');
        self.h.update(&b);
        self.enc.write_all(&b)?;
        self.rows += 1;
        Ok(())
    }
    /// Returns (SHA-256 of the decoded JSONL bytes, rows).
    pub fn finish(self) -> Result<(String, u64)> {
        let w = self.enc.finish()?;
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        fs::rename(&self.tmp, &self.path)?;
        Ok((hex::encode(self.h.finalize()), self.rows))
    }
}

pub fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = tmp_of(path);
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    let mut f = File::create(&tmp)?;
    f.write_all(&b)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?).with_context(|| format!("parse {}", path.display()))
}

/// Rows of a zstd JSONL file.
pub fn rows<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let r = BufReader::with_capacity(1 << 20, zstd::Decoder::new(f)?);
    let mut out = Vec::new();
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        out.push(serde_json::from_str(&l).with_context(|| format!("{} line {}", path.display(), i + 1))?);
    }
    Ok(out)
}

/// Every file under `dir` (recursively) whose name ends with `suffix`, sorted.
pub fn files_with_suffix(dir: &Path, suffix: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d)? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.to_str().is_some_and(|s| s.ends_with(suffix)) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Rows of a plain (uncompressed) JSONL file; blank lines are skipped.
pub fn rows_plain<T: DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let r = BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut out = Vec::new();
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&l).with_context(|| format!("{} line {}", path.display(), i + 1))?);
    }
    Ok(out)
}

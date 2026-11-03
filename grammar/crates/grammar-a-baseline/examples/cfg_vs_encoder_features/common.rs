//! Shared hashing, streaming and output helpers.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
};

pub const PROTOCOL_ID: &str = "cfg-vs-encoder-20261031";
pub const FROZEN_SHA256: &str = "4a33a404b8986a5075fa24e51e1f0c5906ef9c5f970b586b28f0bf3b670944af";

pub fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut input = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn require_hash(path: &Path, expected: &str) -> Result<()> {
    let got = hash_file(path)?;
    ensure!(
        got == expected,
        "sha256 mismatch for {}: expected {expected}, got {got}",
        path.display()
    );
    Ok(())
}

/// Reads newline-terminated lines while hashing the exact file bytes, so a
/// single pass both binds and consumes a large input.
pub struct HashedLines {
    reader: BufReader<File>,
    hash: Sha256,
    buffer: Vec<u8>,
}

impl HashedLines {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            reader: BufReader::with_capacity(
                1 << 22,
                File::open(path).with_context(|| format!("open {}", path.display()))?,
            ),
            hash: Sha256::new(),
            buffer: Vec::new(),
        })
    }

    /// Returns the next line without its trailing newline, or None at EOF.
    pub fn next_line(&mut self) -> Result<Option<&[u8]>> {
        self.buffer.clear();
        let n = self.reader.read_until(b'\n', &mut self.buffer)?;
        if n == 0 {
            return Ok(None);
        }
        self.hash.update(&self.buffer);
        let mut end = self.buffer.len();
        if self.buffer.last() == Some(&b'\n') {
            end -= 1;
        }
        Ok(Some(&self.buffer[..end]))
    }

    pub fn finish(self) -> String {
        hex::encode(self.hash.finalize())
    }
}

pub fn read_frozen(path: &Path) -> Result<Value> {
    let bytes = fs::read(path)?;
    ensure!(
        sha_hex(&bytes) == FROZEN_SHA256,
        "FROZEN.json does not match the frozen protocol hash"
    );
    let value: Value = serde_json::from_slice(&bytes)?;
    ensure!(value["protocol_id"] == PROTOCOL_ID, "protocol id");
    Ok(value)
}

pub fn frozen_str<'a>(frozen: &'a Value, pointer: &str) -> Result<&'a str> {
    frozen
        .pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("FROZEN.json missing {pointer}"))
}

/// Line-oriented JSON writer that hashes exactly what it writes.
pub struct JsonlWriter {
    out: BufWriter<File>,
    hash: Sha256,
    pub rows: usize,
}

impl JsonlWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        Ok(Self {
            out: BufWriter::with_capacity(1 << 20, file),
            hash: Sha256::new(),
            rows: 0,
        })
    }

    pub fn write<T: Serialize>(&mut self, value: &T) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        self.hash.update(&line);
        self.out.write_all(&line)?;
        self.rows += 1;
        Ok(())
    }

    /// Flushes, syncs and returns (rows, sha256).
    pub fn finish(mut self) -> Result<(usize, String)> {
        self.out.flush()?;
        self.out.get_ref().sync_all()?;
        Ok((self.rows, hex::encode(self.hash.finalize())))
    }
}

pub fn write_new_json(path: &Path, value: &Value) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(sha_hex(&bytes))
}

pub fn create_new_dir(path: &Path) -> Result<()> {
    ensure!(!path.try_exists()?, "output must be new: {}", path.display());
    fs::create_dir_all(path)?;
    Ok(())
}

/// Minimal {id, text} request line, the input format shared by the CFG
/// exporter (`jsonl` mode) and the R0 runtime.
#[derive(serde::Deserialize, Serialize)]
pub struct Request {
    pub id: String,
    pub text: String,
}

pub fn read_requests(path: &Path) -> Result<Vec<Request>> {
    let mut lines = HashedLines::open(path)?;
    let mut out = Vec::new();
    while let Some(line) = lines.next_line()? {
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_slice(line)?);
    }
    Ok(out)
}

pub fn now_utc() -> String {
    // Seconds since the epoch; avoids a calendar dependency in this example.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

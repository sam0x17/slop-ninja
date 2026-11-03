//! Local corpus ingestion with exact source spans and reproducible author samples.
//!
//! Dataset bytes and derived databases belong in ignored local storage. Neither
//! author labels nor collection dates prove that every passage is original human
//! writing; copied material is deliberately recorded as unassessed.

pub mod blog;
pub mod export;
pub mod import;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub const SCHEMA: &str = "unslop-blog-corpus-v1";
pub const TOKENIZER: &str = "nfc-lowercase-letter-apostrophe-v1";
pub const DUPLICATE_POLICY: &str = "trim-unicode-whitespace-collapse-whitespace-case-sensitive-v1";
/// Date eligibility is versioned separately from the unchanged SQLite schema.
/// Existing imports retain their original policy and are never silently widened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum DatePolicy {
    #[default]
    #[serde(rename = "valid-supplied-calendar-date-v2")]
    #[value(name = "valid-calendar")]
    ValidCalendar,
    #[serde(rename = "legacy-1999-floor-v1")]
    #[value(name = "legacy-1999")]
    Legacy1999,
}

impl DatePolicy {
    pub fn identity(self) -> &'static str {
        match self {
            Self::ValidCalendar => "valid-supplied-calendar-date-v2",
            Self::Legacy1999 => "legacy-1999-floor-v1",
        }
    }

    pub fn minimum(self) -> Option<&'static str> {
        match self {
            Self::ValidCalendar => None,
            Self::Legacy1999 => Some("1999-01-01"),
        }
    }

    /// Summaries written before policy versioning used the 1999 restriction.
    pub fn legacy() -> Self {
        Self::Legacy1999
    }
}

pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn file_hash(path: &Path) -> Result<String> {
    let mut source = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex::encode(digest.finalize()))
}

pub(crate) fn new_directory(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(path).with_context(|| {
        format!(
            "create {}; refusing to overwrite an existing run",
            path.display()
        )
    })
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut output, value)?;
    output.write_all(b"\n")?;
    Ok(())
}

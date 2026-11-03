use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, io::Write, path::Path};

pub fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

pub fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read {} as UTF-8", path.display()))
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}

pub fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    read(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("{} record {}", path.display(), i + 1))
        })
        .collect()
}

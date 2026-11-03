//! Salted digests for provider bulk, request and task ids in shareable reports.
//!
//! The same contract as `unslop::provider_ids` in the root crate (this crate is
//! a separate workspace): reports carry `redacted-<16 hex>`, the first eight
//! bytes of SHA-256(salt || id). The salt stays in private storage and is read
//! at run time; the raw ids go only to a private sidecar map (mode 600) under
//! `data/` with the columns `provider_id`, `digest` and `file`. A digest passes
//! through unchanged, so raw and digested inputs name the same job.
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const SALT_ENV: &str = "SLOP_NINJA_PROVIDER_ID_SALT";
pub const DEFAULT_SALT: &str = "data/provider-ids/salt";
pub const MAP_ENV: &str = "SLOP_NINJA_PROVIDER_ID_MAP";
pub const DEFAULT_MAP: &str = "data/provider-ids/id-map.private.tsv";
const PREFIX: &str = "redacted-";
const HEADER: &str = "provider_id\tdigest\tfile\n";

pub fn is_digest(value: &str) -> bool {
    value.strip_prefix(PREFIX).is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn salted(salt: &[u8], id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(id.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{PREFIX}{hex}")
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

pub struct ProviderIds {
    salt: Vec<u8>,
    map: PathBuf,
    raw: Mutex<BTreeMap<String, String>>,
}

impl ProviderIds {
    /// Salt from `SLOP_NINJA_PROVIDER_ID_SALT`, else `data/provider-ids/salt`.
    pub fn load() -> Result<Self> {
        let path = env_path(SALT_ENV, DEFAULT_SALT);
        let salt = fs::read(&path).with_context(|| {
            format!(
                "Read the provider-id salt from private storage at {}; set {SALT_ENV} to its file",
                path.display()
            )
        })?;
        Self::new(salt, env_path(MAP_ENV, DEFAULT_MAP))
    }

    pub fn new(salt: Vec<u8>, map: PathBuf) -> Result<Self> {
        ensure!(
            salt.len() >= 16,
            "The provider-id salt needs at least 16 bytes"
        );
        Ok(Self {
            salt,
            map,
            raw: Mutex::new(BTreeMap::new()),
        })
    }

    /// The shareable form; a digest passes through unchanged.
    pub fn shareable(&self, id: &str) -> String {
        if is_digest(id) {
            return id.to_owned();
        }
        let digest = salted(&self.salt, id);
        self.raw
            .lock()
            .expect("provider-id map lock")
            .insert(digest.clone(), id.to_owned());
        digest
    }

    /// Append the raw ids digested so far, with `file`, to the private map.
    pub fn record(&self, file: &str) -> Result<usize> {
        let raw = self.raw.lock().expect("provider-id map lock").clone();
        if raw.is_empty() {
            return Ok(0);
        }
        let clean = |s: &str| !s.is_empty() && !s.contains(['\t', '\n', '\r']);
        ensure!(clean(file), "Sidecar file label must be one nonempty field");
        ensure!(
            raw.values().all(|id| clean(id)),
            "A provider id cannot be stored as one sidecar field"
        );
        if let Some(parent) = self.map.parent().filter(|p| !p.as_os_str().is_empty()) {
            create_private_dir(parent)?;
        }
        let existing = match fs::read_to_string(&self.map) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", self.map.display())),
        };
        let present: BTreeSet<&str> = existing.lines().collect();
        let mut rows = String::new();
        if existing.is_empty() {
            rows.push_str(HEADER);
        }
        let mut added = 0;
        for (digest, id) in &raw {
            let row = format!("{id}\t{digest}\t{file}");
            if !present.contains(row.as_str()) {
                rows.push_str(&row);
                rows.push('\n');
                added += 1;
            }
        }
        let mut out = open_private_append(&self.map)?;
        if added > 0 {
            out.write_all(rows.as_bytes())?;
            out.sync_all()?;
        }
        Ok(added)
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("create {}", path.display()))
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))
}

#[cfg(unix)]
fn open_private_append(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    if file.metadata()?.permissions().mode() & 0o077 != 0 {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_private_append(path: &Path) -> Result<fs::File> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_the_root_crate_vector_and_sidecar_is_private() {
        let root = std::env::temp_dir().join(format!(
            "slop-ninja-provider-ids-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let map = root.join("private").join("map.tsv");
        let ids = ProviderIds::new(b"synthetic-provider-id-salt-v1".to_vec(), map.clone()).unwrap();
        // Same vector as the root crate's test (pinned with sha256sum).
        let digest = ids.shareable("synthetic-task-0001");
        assert_eq!(digest, "redacted-6730f8b88427cc9b");
        assert_eq!(ids.shareable(&digest), digest);
        assert!(!is_digest("synthetic-task-0001"));
        assert_eq!(ids.record("report.json").unwrap(), 1);
        assert_eq!(ids.record("report.json").unwrap(), 0);
        let text = fs::read_to_string(&map).unwrap();
        assert_eq!(
            text,
            format!("{HEADER}synthetic-task-0001\t{digest}\treport.json\n")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&map).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(&root).unwrap();
    }
}

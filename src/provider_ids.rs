//! Salted digests for provider job, request and task ids in shareable results.
//!
//! Results that may be tracked carry `redacted-<16 hex>`: the first eight bytes
//! of SHA-256(salt || id), the digest form already used in the tracked results.
//! The salt stays in private storage and is read at run time. The raw ids of a
//! result go only to a private sidecar map (mode 600) under `data/`, with the
//! columns `provider_id`, `digest` and `file`. Raw receipts that resume or poll
//! a provider job keep their raw ids and belong in private storage.
//!
//! Readers accept both forms: a digest passes through [`ProviderIds::shareable`]
//! unchanged, so results written with raw ids and with digests compare equal.

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};
use zeroize::Zeroizing;

/// Overrides [`DEFAULT_SALT`].
pub const SALT_ENV: &str = "SLOP_NINJA_PROVIDER_ID_SALT";
/// Private salt file, relative to the working directory.
pub const DEFAULT_SALT: &str = "data/provider-ids/salt";
/// Overrides [`DEFAULT_MAP`].
pub const MAP_ENV: &str = "SLOP_NINJA_PROVIDER_ID_MAP";
/// Private raw-id sidecar map, relative to the working directory.
pub const DEFAULT_MAP: &str = "data/provider-ids/id-map.private.tsv";
pub const PREFIX: &str = "redacted-";
const HEADER: &str = "provider_id\tdigest\tfile\n";

/// True for the shareable form `redacted-<16 lowercase hex>`.
pub fn is_digest(value: &str) -> bool {
    value.strip_prefix(PREFIX).is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn salted(salt: &[u8], id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(id.as_bytes());
    format!("{PREFIX}{}", hex::encode(&hasher.finalize()[..8]))
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// One run's salt, sidecar location and the raw ids it has digested.
pub struct ProviderIds {
    salt: Zeroizing<Vec<u8>>,
    map: PathBuf,
    raw: Mutex<BTreeMap<String, String>>,
}

impl ProviderIds {
    /// Read the salt from private storage (`SLOP_NINJA_PROVIDER_ID_SALT`, else
    /// `data/provider-ids/salt`). Load it before any paid request so a missing
    /// salt cannot strand a submitted job.
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

    /// An explicit salt and sidecar map, for tests and other tools.
    pub fn new(salt: Vec<u8>, map: PathBuf) -> Result<Self> {
        let salt = Zeroizing::new(salt);
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

    pub fn map(&self) -> &Path {
        &self.map
    }

    /// The shareable form of a provider id. A digest passes through unchanged;
    /// a raw id is digested and kept for [`ProviderIds::record`].
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

    /// Append each raw id digested so far, with `file` (the result path, or the
    /// command when the result goes to standard output), to the sidecar map.
    /// Rows already present are skipped. Returns the number of new rows.
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
            Err(e) => {
                return Err(e).with_context(|| format!("read {}", self.map.display()));
            }
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

    const SALT: &[u8] = b"synthetic-provider-id-salt-v1";

    #[test]
    fn digest_matches_the_published_form_and_passes_through() {
        let root = tempfile::tempdir().unwrap();
        let ids = ProviderIds::new(SALT.to_vec(), root.path().join("map.tsv")).unwrap();
        let digest = ids.shareable("synthetic-task-0001");
        // sha256(salt || id), first eight bytes; pinned with sha256sum.
        assert_eq!(digest, "redacted-6730f8b88427cc9b");
        assert!(is_digest(&digest));
        assert_eq!(ids.shareable(&digest), digest);
        assert_eq!(
            salted(b"salt", "0f0f0f0f-1111-4222-8333-444455556666"),
            "redacted-4fbad18eb4d7b4c2"
        );
        for not in [
            "redacted-6730F8B88427CC9B",
            "redacted-6730f8b8",
            "synthetic-task-0001",
        ] {
            assert!(!is_digest(not), "{not}");
        }
        assert!(ProviderIds::new(b"short".to_vec(), root.path().join("m")).is_err());
    }

    #[test]
    fn sidecar_is_private_deduplicated_and_skips_digests() {
        let root = tempfile::tempdir().unwrap();
        let map = root.path().join("private").join("map.tsv");
        let ids = ProviderIds::new(SALT.to_vec(), map.clone()).unwrap();
        assert_eq!(ids.record("result.json").unwrap(), 0);
        assert!(!map.exists());
        let digest = ids.shareable("synthetic-task-0001");
        ids.shareable("redacted-0011223344556677");
        assert_eq!(ids.record("result.json").unwrap(), 1);
        assert_eq!(ids.record("result.json").unwrap(), 0);
        assert_eq!(ids.record("other.json").unwrap(), 1);
        let text = fs::read_to_string(&map).unwrap();
        assert_eq!(
            text,
            format!(
                "{HEADER}synthetic-task-0001\t{digest}\tresult.json\nsynthetic-task-0001\t{digest}\tother.json\n"
            )
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&map).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(map.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            fs::set_permissions(&map, fs::Permissions::from_mode(0o644)).unwrap();
            ids.shareable("synthetic-task-0002");
            assert_eq!(ids.record("result.json").unwrap(), 1);
            assert_eq!(
                fs::metadata(&map).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        ids.shareable("synthetic\ttask");
        assert!(ids.record("result.json").is_err());
    }
}
